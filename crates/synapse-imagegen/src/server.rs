use secrecy::SecretString;
use synapse_config::{FailMode, ImageGenProviderConfig, ImageGenProviderType};
use synapse_core::RequestContext;

use crate::{
    error::ImageGenError,
    provider::{ImageGenProvider, openai::OpenAiImageGenProvider},
    types::{ImageRequest, ImageResponse},
};

/// Image generation server that routes requests to the appropriate provider
pub struct Server {
    providers: Vec<Box<dyn ImageGenProvider>>,
    /// Usage recorder for billing metering
    #[cfg(feature = "billing")]
    usage_recorder: Option<synapse_billing::UsageRecorder>,
    /// Billing client for pre-request credit checks
    #[cfg(feature = "billing")]
    billing_client: Option<synapse_billing::AetherClient>,
    /// Behavior when billing service is unreachable
    #[cfg(feature = "billing")]
    billing_fail_mode: FailMode,
    #[cfg(feature = "billing")]
    margins: synapse_core::MarginPolicy,
}

impl Server {
    /// Attach a usage recorder for billing metering
    ///
    /// Must be called before the server is shared with handlers.
    #[cfg(feature = "billing")]
    pub fn set_usage_recorder(&mut self, recorder: synapse_billing::UsageRecorder) {
        self.usage_recorder = Some(recorder);
    }

    /// Attach a billing client for pre-request credit checks
    ///
    /// Must be called before the server is shared with handlers.
    #[cfg(feature = "billing")]
    pub fn set_billing_client(
        &mut self,
        client: synapse_billing::AetherClient,
        fail_mode: FailMode,
        margins: synapse_core::MarginPolicy,
    ) {
        self.billing_client = Some(client);
        self.billing_fail_mode = fail_mode;
        self.margins = margins;
    }

    /// Amount to charge the request's billing identity for usage costing
    /// `base_usd` upstream, marked up for the billed product
    #[cfg(feature = "billing")]
    fn charge(&self, context: &RequestContext, provider_name: &str, base_usd: f64) -> f64 {
        context
            .billing_identity
            .as_ref()
            .map_or(0.0, |identity| self.margins.charge(identity, provider_name, base_usd))
    }

    /// Generate images using the appropriate provider
    ///
    /// Routes to a provider based on the model name in the request.
    /// Model format: "provider/model" (e.g. "openai/dall-e-3").
    /// If no provider prefix, uses the first configured provider.
    pub async fn generate(
        &self,
        request: &ImageRequest,
        context: &RequestContext,
    ) -> crate::error::Result<ImageResponse> {
        // Extract provider name from model (format: "provider/model")
        let (provider_name, _model_name) = request.model.split_once('/').unwrap_or(("", &request.model));

        let provider = if provider_name.is_empty() {
            // If no provider prefix, use the first provider
            self.providers.first().ok_or_else(|| {
                ImageGenError::ProviderNotFound("No image generation providers configured".to_string())
            })?
        } else {
            self.providers
                .iter()
                .find(|p| p.name() == provider_name)
                .ok_or_else(|| ImageGenError::ProviderNotFound(provider_name.to_string()))?
        };

        // Pre-request spend check for managed billing
        #[cfg(feature = "billing")]
        {
            let estimated_cost = self.charge(context, provider.name(), Self::estimate_image_cost(request));
            if estimated_cost > 0.0 {
                self.check_spend(context, estimated_cost).await?;
            }
        }

        let response = provider.generate(request, context).await?;

        // Post-request billing: record usage and charge the actual cost
        #[cfg(feature = "billing")]
        {
            let pname = if provider_name.is_empty() {
                provider.name()
            } else {
                provider_name
            };
            let request_id = uuid::Uuid::new_v4().to_string();
            self.record_usage(context, pname, request, &request_id);
            self.settle_spend(context, pname, request, &request_id).await;
        }

        Ok(response)
    }

    /// Estimate the cost of an image generation request
    #[cfg(feature = "billing")]
    fn estimate_image_cost(request: &ImageRequest) -> f64 {
        // Base cost per image varies by size and quality
        #[allow(clippy::match_same_arms)]
        let per_image = match (request.size.as_str(), request.quality.as_str()) {
            ("1024x1024", "hd") => 0.080,
            ("1024x1792" | "1792x1024", "hd") => 0.120,
            ("1024x1792" | "1792x1024", _) => 0.080,
            ("512x512", _) => 0.018,
            ("256x256", _) => 0.016,
            _ => 0.040,
        };
        per_image * f64::from(request.n)
    }

    /// Check that the estimated cost is covered: by the credit wallet, or by
    /// the product's included allowance when it includes AI in its tier
    #[cfg(feature = "billing")]
    async fn check_spend(&self, context: &RequestContext, estimated_cost: f64) -> crate::error::Result<()> {
        let Some(ref client) = self.billing_client else {
            return Ok(());
        };

        let Some(ref identity) = context.billing_identity else {
            return Ok(());
        };

        if identity.mode != synapse_core::BillingMode::Managed {
            return Ok(());
        }

        match client
            .check_spend(
                &identity.app_id,
                &identity.entity_type,
                &identity.entity_id,
                estimated_cost,
            )
            .await
        {
            Ok(synapse_billing::SpendCheck::Allowed) => Ok(()),
            Ok(synapse_billing::SpendCheck::InsufficientCredits { balance }) => {
                Err(ImageGenError::InvalidRequest(format!(
                    "insufficient credits: estimated cost ${estimated_cost:.4} exceeds available balance ${balance:.4}"
                )))
            }
            Ok(synapse_billing::SpendCheck::AllowanceExhausted) => Err(ImageGenError::AllowanceExhausted),
            Err(e) => match self.billing_fail_mode {
                FailMode::Degraded => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, allowing image generation request (degraded mode)"
                    );
                    Ok(())
                }
                FailMode::Open => {
                    tracing::warn!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, allowing image generation request (fail-open mode)"
                    );
                    Ok(())
                }
                FailMode::Closed => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, rejecting image generation request (fail-closed mode)"
                    );
                    Err(ImageGenError::BillingUnavailable)
                }
            },
        }
    }

    /// Record usage for billing metering
    #[cfg(feature = "billing")]
    fn record_usage(&self, context: &RequestContext, provider_name: &str, request: &ImageRequest, request_id: &str) {
        let Some(ref recorder) = self.usage_recorder else {
            return;
        };

        let Some(ref identity) = context.billing_identity else {
            return;
        };

        let is_byok = identity.mode == synapse_core::BillingMode::Byok;

        // Use input_tokens to track image count for imagegen
        // For BYOK, zero out tokens and cost; only the request count is metered
        recorder.record(synapse_billing::UsageEvent {
            app_id: identity.app_id.clone(),
            entity_type: identity.entity_type.clone(),
            entity_id: identity.entity_id.clone(),
            model: request.model.clone(),
            provider: provider_name.to_owned(),
            input_tokens: if is_byok { 0 } else { request.n },
            output_tokens: 0,
            estimated_cost_usd: self.charge(context, provider_name, Self::estimate_image_cost(request)),
            idempotency_key: request_id.to_owned(),
        });
    }

    /// Charge the actual cost after successful image generation to the credit wallet, or to the
    /// product's included allowance
    #[cfg(feature = "billing")]
    async fn settle_spend(
        &self,
        context: &RequestContext,
        provider_name: &str,
        request: &ImageRequest,
        request_id: &str,
    ) {
        let Some(ref client) = self.billing_client else {
            return;
        };

        let Some(ref identity) = context.billing_identity else {
            return;
        };

        if identity.mode != synapse_core::BillingMode::Managed {
            return;
        }

        let actual_cost = self.charge(context, provider_name, Self::estimate_image_cost(request));

        if actual_cost <= 0.0 {
            return;
        }

        let charge = synapse_billing::SpendCharge {
            amount_usd: actual_cost,
            description: format!("{provider_name}/{}", request.model),
            idempotency_key: request_id.to_owned(),
            reference_type: "image_generation".to_owned(),
        };

        if let Err(e) = client
            .settle_spend(&identity.app_id, &identity.entity_type, &identity.entity_id, &charge)
            .await
        {
            tracing::warn!(
                error = %e,
                entity_id = %identity.entity_id,
                cost = actual_cost,
                "failed to charge usage after image generation"
            );
        }
    }
}

/// Builder for constructing the image generation server from configuration
pub struct ImageGenServerBuilder<'a> {
    config: &'a synapse_config::Config,
}

impl<'a> ImageGenServerBuilder<'a> {
    pub fn new(config: &'a synapse_config::Config) -> Self {
        Self { config }
    }

    pub fn build(self) -> crate::error::Result<Server> {
        let mut providers: Vec<Box<dyn ImageGenProvider>> = Vec::new();

        for (name, provider_config) in &self.config.imagegen.providers {
            tracing::debug!("Initializing image generation provider: {name}");

            let provider: Box<dyn ImageGenProvider> = match &provider_config.provider_type {
                ImageGenProviderType::Openai => {
                    let api_key = resolve_api_key(name, provider_config)?;

                    Box::new(OpenAiImageGenProvider::new(
                        name.clone(),
                        api_key,
                        provider_config.base_url.clone(),
                    ))
                }
            };

            providers.push(provider);
        }

        if providers.is_empty() {
            tracing::debug!("No image generation providers configured");
        } else {
            tracing::debug!(
                "Image generation server initialized with {} provider(s)",
                providers.len()
            );
        }

        Ok(Server {
            providers,
            #[cfg(feature = "billing")]
            usage_recorder: None,
            #[cfg(feature = "billing")]
            billing_client: None,
            #[cfg(feature = "billing")]
            billing_fail_mode: FailMode::default(),
            #[cfg(feature = "billing")]
            margins: synapse_core::MarginPolicy::default(),
        })
    }
}

fn resolve_api_key(name: &str, config: &ImageGenProviderConfig) -> crate::error::Result<SecretString> {
    config
        .api_key
        .clone()
        .ok_or_else(|| ImageGenError::ConfigError(format!("API key required for image generation provider '{name}'")))
}

#[cfg(all(test, feature = "billing"))]
mod spend_tests {
    use std::collections::HashMap;

    use secrecy::SecretString;
    use synapse_billing::{AetherClient, SpendPolicy};
    use synapse_config::FailMode;
    use synapse_core::{BillingIdentity, BillingMode, MarginPolicy, RequestContext};
    use wiremock::matchers::{method, path, path_regex, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::Server;
    use crate::error::ImageGenError;
    use crate::provider::openai::OpenAiImageGenProvider;
    use crate::types::ImageRequest;

    fn server(provider: &MockServer, aether: &MockServer) -> Server {
        let client = AetherClient::new(
            reqwest::Url::parse(&format!("{}/", aether.uri())).unwrap(),
            SecretString::from("aether-key".to_owned()),
        )
        .unwrap()
        .with_spend_policy(SpendPolicy::new(HashMap::from([(
            "myfi".to_owned(),
            "ai_credits".to_owned(),
        )])));

        Server {
            providers: vec![Box::new(OpenAiImageGenProvider::new(
                "openai".to_owned(),
                SecretString::from("provider-key".to_owned()),
                Some(format!("{}/v1", provider.uri())),
            ))],
            usage_recorder: None,
            billing_client: Some(client),
            billing_fail_mode: FailMode::Degraded,
            margins: MarginPolicy::default(),
        }
    }

    fn myfi_context() -> RequestContext {
        let mut ctx = RequestContext::empty();
        ctx.billing_identity = Some(BillingIdentity {
            app_id: "myfi".to_owned(),
            entity_type: "organization".to_owned(),
            entity_id: "org_1".to_owned(),
            mode: BillingMode::Managed,
            plan: None,
        });
        ctx
    }

    fn request() -> ImageRequest {
        serde_json::from_value(serde_json::json!({ "prompt": "a receipt", "model": "openai/gpt-image-1" })).unwrap()
    }

    async fn forbid_wallet(aether: &MockServer) {
        Mock::given(path_regex(r"^/credits/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .named("an included-allowance product must never touch the credit wallet")
            .mount(aether)
            .await;
    }

    async fn mount_provider(provider: &MockServer, times: u64) {
        Mock::given(method("POST"))
            .and(path("/v1/images/generations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "created": 1, "data": [{ "url": "https://img.test/1.png" }]
            })))
            .expect(times)
            .mount(provider)
            .await;
    }

    // One standard 1024x1024 image is $0.04 upstream, $0.08 retail at the 2.0
    // default product margin: 8 credits
    #[tokio::test]
    async fn allowance_product_is_checked_and_charged_in_credits_never_the_wallet() {
        let (provider, aether) = (MockServer::start().await, MockServer::start().await);
        mount_provider(&provider, 1).await;
        forbid_wallet(&aether).await;
        Mock::given(method("GET"))
            .and(path("/usage/myfi/organization/org_1/ai_credits/check"))
            .and(query_param("additionalUsage", "8"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "allowed": true })))
            .expect(1)
            .mount(&aether)
            .await;
        Mock::given(method("POST"))
            .and(path("/usage/myfi/organization/org_1/ai_credits/record"))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({ "delta": 8.0 }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "eventId": "e" })))
            .expect(1)
            .mount(&aether)
            .await;

        server(&provider, &aether)
            .generate(&request(), &myfi_context())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn exhausted_allowance_is_refused_with_402_before_the_provider_is_called() {
        let (provider, aether) = (MockServer::start().await, MockServer::start().await);
        mount_provider(&provider, 0).await;
        forbid_wallet(&aether).await;
        Mock::given(method("GET"))
            .and(path("/usage/myfi/organization/org_1/ai_credits/check"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "allowed": false })))
            .expect(1)
            .mount(&aether)
            .await;

        let err = server(&provider, &aether)
            .generate(&request(), &myfi_context())
            .await
            .unwrap_err();

        assert!(matches!(err, ImageGenError::AllowanceExhausted));
        assert_eq!(err.status_code(), axum::http::StatusCode::PAYMENT_REQUIRED);
    }
}
