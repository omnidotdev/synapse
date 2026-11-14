use secrecy::SecretString;
use synapse_config::{FailMode, ImageGenProviderConfig, ImageGenProviderType};
use synapse_core::RequestContext;

#[cfg(feature = "billing")]
use crate::pricing::{EstimateInput, GptImagePrice, gpt_image_price};
use crate::{
    edit::ImageEditRequest,
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

    /// Provider for a model id
    ///
    /// Model format: "provider/model" (e.g. "openai/gpt-image-1.5"). If no
    /// provider prefix, uses the first configured provider
    fn provider_for(&self, model: &str) -> crate::error::Result<&dyn ImageGenProvider> {
        let provider_name = model.split_once('/').map_or("", |(provider, _)| provider);

        let provider = if provider_name.is_empty() {
            self.providers.first().ok_or_else(|| {
                ImageGenError::ProviderNotFound("No image generation providers configured".to_string())
            })?
        } else {
            self.providers
                .iter()
                .find(|p| p.name() == provider_name)
                .ok_or_else(|| ImageGenError::ProviderNotFound(provider_name.to_string()))?
        };

        Ok(provider.as_ref())
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
        let provider = self.provider_for(&request.model)?;
        let mut context = context.clone();
        resolve_byok_key(&mut context, provider.key_provider())?;

        // Pre-request spend check for managed billing
        #[cfg(feature = "billing")]
        {
            let estimated_cost = self.charge(&context, provider.key_provider(), Self::estimate_image_cost(request));
            if estimated_cost > 0.0 {
                self.check_spend(&context, estimated_cost).await?;
            }
        }

        let response = provider.generate(request, &context).await?;

        // Post-request billing: record usage and charge the actual cost
        #[cfg(feature = "billing")]
        {
            let base_cost = Self::generation_cost(request, &response);
            let billed = Billed {
                provider_name: provider.name(),
                vendor: provider.key_provider(),
                model: &request.model,
                images: request.n,
                base_cost,
                modality: synapse_core::Modality::ImageGeneration,
                request_id: uuid::Uuid::new_v4().to_string(),
            };
            self.record_usage(&context, &billed);
            self.settle_spend(&context, &billed).await;
            self.report_usage(&context, &billed);
        }

        Ok(response)
    }

    /// Edit or compose images from reference images
    ///
    /// Routed and keyed like [`Self::generate`]. A managed request is priced
    /// from the model's published GPT Image rates and refused when the model
    /// has none, spend-checked before the provider is called, and charged from
    /// the token usage the provider reports
    pub async fn edit(
        &self,
        request: &ImageEditRequest,
        context: &RequestContext,
    ) -> crate::error::Result<ImageResponse> {
        let provider = self.provider_for(&request.model)?;
        let mut context = context.clone();
        resolve_byok_key(&mut context, provider.key_provider())?;

        #[cfg(feature = "billing")]
        let managed = Self::managed_edit_estimate(&context, provider.name(), request)?;
        #[cfg(feature = "billing")]
        if let Some((_, estimate)) = managed {
            self.check_spend(&context, self.charge(&context, provider.key_provider(), estimate))
                .await?;
        }

        let response = provider.edit(request, &context).await?;

        #[cfg(feature = "billing")]
        {
            let base_cost = managed.map_or(0.0, |(price, estimate)| {
                response.usage.as_ref().map_or(estimate, |usage| price.usage_usd(usage))
            });
            let billed = Billed {
                provider_name: provider.name(),
                vendor: provider.key_provider(),
                model: &request.model,
                images: request.n,
                base_cost,
                modality: synapse_core::Modality::ImageEdit,
                request_id: uuid::Uuid::new_v4().to_string(),
            };
            self.record_usage(&context, &billed);
            self.settle_spend(&context, &billed).await;
            self.report_usage(&context, &billed);
        }

        Ok(response)
    }

    /// Price and upstream cost estimate of a managed edit (`None` when the
    /// request is not managed, so nothing is charged)
    ///
    /// # Errors
    ///
    /// Returns `ModelNotPriced` for a managed request whose model has no
    /// published price: serving it on Omni's key would bill nothing
    #[cfg(feature = "billing")]
    fn managed_edit_estimate(
        context: &RequestContext,
        provider_name: &str,
        request: &ImageEditRequest,
    ) -> crate::error::Result<Option<(&'static GptImagePrice, f64)>> {
        let Some(identity) = context.billing_identity.as_ref() else {
            return Ok(None);
        };
        if identity.mode != synapse_core::BillingMode::Managed {
            return Ok(None);
        }

        let model = bare_model(&request.model);
        let price = gpt_image_price(model).ok_or_else(|| ImageGenError::ModelNotPriced {
            model: format!("{provider_name}/{model}"),
        })?;

        let input_images = u32::try_from(request.image_count + usize::from(request.has_mask)).unwrap_or(u32::MAX);
        let estimate = price.estimate_usd(&EstimateInput {
            quality: request.quality.as_deref(),
            size: request.size.as_deref(),
            n: request.n,
            input_images,
            high_fidelity: request.input_fidelity.as_deref() == Some("high"),
            prompt_chars: request.prompt.chars().count(),
        });

        Ok(Some((price, estimate)))
    }

    /// Estimate the cost of an image generation request
    ///
    /// GPT Image models use their published prices; other models fall back
    /// to a flat per-size table
    #[cfg(feature = "billing")]
    fn estimate_image_cost(request: &ImageRequest) -> f64 {
        if let Some(price) = gpt_image_price(bare_model(&request.model)) {
            return price.estimate_usd(&EstimateInput {
                quality: Some(&request.quality),
                size: Some(&request.size),
                n: request.n,
                input_images: 0,
                high_fidelity: false,
                prompt_chars: request.prompt.chars().count(),
            });
        }

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

    /// Upstream cost of a served generation: the reported token usage for a
    /// GPT Image model, else the estimate
    #[cfg(feature = "billing")]
    fn generation_cost(request: &ImageRequest, response: &ImageResponse) -> f64 {
        match (gpt_image_price(bare_model(&request.model)), response.usage.as_ref()) {
            (Some(price), Some(usage)) => price.usage_usd(usage),
            _ => Self::estimate_image_cost(request),
        }
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
                Err(ImageGenError::InsufficientCredits(format!(
                    "estimated cost ${estimated_cost:.4} exceeds available balance ${balance:.4}"
                )))
            }
            Ok(synapse_billing::SpendCheck::AllowanceExhausted) => Err(ImageGenError::AllowanceExhausted),
            Err(e) => match self.billing_fail_mode {
                FailMode::Degraded => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, allowing image request (degraded mode)"
                    );
                    Ok(())
                }
                FailMode::Open => {
                    tracing::warn!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, allowing image request (fail-open mode)"
                    );
                    Ok(())
                }
                FailMode::Closed => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, rejecting image request (fail-closed mode)"
                    );
                    Err(ImageGenError::BillingUnavailable)
                }
            },
        }
    }

    /// Record usage for billing metering
    #[cfg(feature = "billing")]
    fn record_usage(&self, context: &RequestContext, billed: &Billed<'_>) {
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
            model: billed.model.to_owned(),
            provider: billed.provider_name.to_owned(),
            input_tokens: if is_byok { 0 } else { billed.images },
            output_tokens: 0,
            estimated_cost_usd: self.charge(context, billed.vendor, billed.base_cost),
            idempotency_key: billed.request_id.clone(),
        });
    }

    /// Charge the actual cost of a served request to the credit wallet, or to
    /// the product's included allowance
    #[cfg(feature = "billing")]
    async fn settle_spend(&self, context: &RequestContext, billed: &Billed<'_>) {
        let Some(ref client) = self.billing_client else {
            return;
        };

        let Some(ref identity) = context.billing_identity else {
            return;
        };

        if identity.mode != synapse_core::BillingMode::Managed {
            return;
        }

        let actual_cost = self.charge(context, billed.vendor, billed.base_cost);

        if actual_cost <= 0.0 {
            return;
        }

        let charge = synapse_billing::SpendCharge {
            amount_usd: actual_cost,
            description: format!("{}/{}", billed.provider_name, bare_model(billed.model)),
            idempotency_key: billed.request_id.clone(),
            reference_type: billed.modality.as_str().to_owned(),
        };

        if let Err(e) = client
            .settle_spend(&identity.app_id, &identity.entity_type, &identity.entity_id, &charge)
            .await
        {
            tracing::warn!(
                error = %e,
                entity_id = %identity.entity_id,
                cost = actual_cost,
                reference_type = billed.modality.as_str(),
                "failed to charge usage after image request"
            );
        }
    }

    /// Report a served request to synapse-api for dashboard charts and
    /// reconciliation, carrying exactly what it was charged
    #[cfg(feature = "billing")]
    fn report_usage(&self, context: &RequestContext, billed: &Billed<'_>) {
        synapse_core::report_served_usage(
            &context.parts.extensions,
            synapse_core::ServedUsage {
                modality: billed.modality,
                provider: billed.provider_name.to_owned(),
                model: bare_model(billed.model).to_owned(),
                input_tokens: billed.images,
                output_tokens: 0,
                charged_usd: synapse_billing::round_up_to_ledger_precision(self.charge(
                    context,
                    billed.vendor,
                    billed.base_cost,
                )),
            },
        );
    }
}

/// A served request to meter and charge
#[cfg(feature = "billing")]
struct Billed<'a> {
    provider_name: &'a str,
    /// Vendor the provider calls, which its margin is resolved by (like its
    /// BYOK key)
    vendor: &'static str,
    model: &'a str,
    /// Images requested
    images: u32,
    /// Upstream cost in USD, before the product margin
    base_cost: f64,
    /// Kind of usage, also the charge's reference type (e.g. `image_edit`)
    modality: synapse_core::Modality,
    request_id: String,
}

/// Model id without its "provider/" prefix
#[cfg(feature = "billing")]
fn bare_model(model: &str) -> &str {
    model.split_once('/').map_or(model, |(_, name)| name)
}

/// Point a BYOK request at the caller's own provider key
///
/// A BYOK request is never charged, so it must never be served on Omni's
/// key: it uses the key registered for this provider and is refused when
/// there is none. A key forwarded in a request header is ignored, as on every
/// other endpoint. Managed and unauthenticated requests keep the provider's
/// configured key
fn resolve_byok_key(context: &mut RequestContext, provider_name: &str) -> crate::error::Result<()> {
    let is_byok = context
        .billing_identity
        .as_ref()
        .is_some_and(|identity| identity.mode == synapse_core::BillingMode::Byok);
    if !is_byok {
        return Ok(());
    }

    let key = context.provider_keys.get(provider_name).cloned().ok_or_else(|| {
        ImageGenError::InvalidRequest(format!(
            "no BYOK key registered for provider '{provider_name}'. \
             Register one via setProviderKey or use a managed-mode API key"
        ))
    })?;
    context.api_key = Some(key);

    Ok(())
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

    use axum::body::Bytes;
    use secrecy::SecretString;
    use synapse_billing::{AetherClient, SpendPolicy};
    use synapse_config::FailMode;
    use synapse_core::{BillingIdentity, BillingMode, MarginPolicy, RequestContext};
    use wiremock::matchers::{header, method, path, path_regex, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::Server;
    use crate::edit::{FormPart, ImageEditRequest};
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

    fn context(app_id: &str, mode: BillingMode) -> RequestContext {
        let mut ctx = RequestContext::empty();
        ctx.billing_identity = Some(BillingIdentity {
            app_id: app_id.to_owned(),
            entity_type: "user".to_owned(),
            entity_id: "u1".to_owned(),
            mode,
            plan: None,
        });
        ctx
    }

    #[tokio::test]
    async fn wallet_shortfall_is_refused_with_402_before_the_provider_is_called() {
        let (provider, aether) = (MockServer::start().await, MockServer::start().await);
        mount_provider(&provider, 0).await;
        Mock::given(method("GET"))
            .and(path("/credits/creator/user/u1/check"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "sufficient": false, "balance": 0.01 })),
            )
            .expect(1)
            .mount(&aether)
            .await;

        let err = server(&provider, &aether)
            .generate(&request(), &context("creator", BillingMode::Managed))
            .await
            .unwrap_err();

        assert!(matches!(err, ImageGenError::InsufficientCredits(_)));
        assert_eq!(err.status_code(), axum::http::StatusCode::PAYMENT_REQUIRED);
    }

    #[tokio::test]
    async fn byok_generation_uses_the_callers_key_and_is_never_charged() {
        let (provider, aether) = (MockServer::start().await, MockServer::start().await);
        Mock::given(method("POST"))
            .and(path("/v1/images/generations"))
            .and(header("authorization", "Bearer sk-user-own"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "created": 1, "data": [{ "url": "https://img.test/1.png" }]
            })))
            .expect(1)
            .mount(&provider)
            .await;
        Mock::given(path_regex(r"^/(credits|usage)/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&aether)
            .await;

        let mut ctx = context("creator", BillingMode::Byok);
        ctx.provider_keys
            .insert("openai".to_owned(), SecretString::from("sk-user-own".to_owned()));

        server(&provider, &aether).generate(&request(), &ctx).await.unwrap();
    }

    #[tokio::test]
    async fn byok_generation_without_a_provider_key_is_refused_not_served_on_the_gateway_key() {
        let (provider, aether) = (MockServer::start().await, MockServer::start().await);
        mount_provider(&provider, 0).await;

        let err = server(&provider, &aether)
            .generate(&request(), &context("creator", BillingMode::Byok))
            .await
            .unwrap_err();

        assert!(matches!(err, ImageGenError::InvalidRequest(_)));
    }

    /// BYOK runs only on the key registered for the provider, as on every
    /// other endpoint: a key forwarded in a request header is ignored
    #[tokio::test]
    async fn byok_generation_ignores_a_provider_key_header() {
        let (provider, aether) = (MockServer::start().await, MockServer::start().await);
        mount_provider(&provider, 0).await;

        let mut ctx = context("creator", BillingMode::Byok);
        ctx.parts
            .headers
            .insert("x-provider-api-key", axum::http::HeaderValue::from_static("sk-header"));
        let err = server(&provider, &aether).generate(&request(), &ctx).await.unwrap_err();

        assert!(matches!(err, ImageGenError::InvalidRequest(_)));
    }

    // 4,000 output tokens at gpt-image-1.5's $32/M plus 10 text input tokens
    // at $5/M is $0.12805 upstream, $0.2561 at the 2.0 product margin
    #[tokio::test]
    async fn gpt_image_generation_is_charged_from_reported_usage() {
        let (provider, aether) = (MockServer::start().await, MockServer::start().await);
        // Managed requests are served on the configured key
        Mock::given(method("POST"))
            .and(path("/v1/images/generations"))
            .and(header("authorization", "Bearer provider-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "created": 1,
                "data": [{ "b64_json": "aW1n" }],
                "usage": {
                    "input_tokens": 10,
                    "input_tokens_details": { "image_tokens": 0, "text_tokens": 10 },
                    "output_tokens": 4000,
                    "total_tokens": 4010
                }
            })))
            .expect(1)
            .mount(&provider)
            .await;
        Mock::given(method("GET"))
            .and(path("/credits/creator/user/u1/check"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "sufficient": true, "balance": 5.0 })),
            )
            .expect(1)
            .mount(&aether)
            .await;
        Mock::given(method("POST"))
            .and(path("/credits/creator/user/u1/deduct"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "amount": 0.2561,
                "referenceType": "image_generation",
                "description": "openai/gpt-image-1.5"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "success": true })))
            .expect(1)
            .mount(&aether)
            .await;

        let request: ImageRequest = serde_json::from_value(serde_json::json!({
            "prompt": "a fox", "model": "openai/gpt-image-1.5", "quality": "high"
        }))
        .unwrap();

        let response = server(&provider, &aether)
            .generate(&request, &context("creator", BillingMode::Managed))
            .await
            .unwrap();

        assert_eq!(response.usage.unwrap().output_tokens, 4000);
    }

    fn edit_request(model: &str) -> ImageEditRequest {
        let field = |name: &str, value: &str| FormPart {
            name: name.to_owned(),
            file_name: None,
            content_type: None,
            data: Bytes::from(value.to_owned()),
        };
        ImageEditRequest::from_parts(vec![
            field("model", model),
            field("prompt", "a fox"),
            FormPart {
                name: "image[]".to_owned(),
                file_name: Some("ref.png".to_owned()),
                content_type: Some("image/png".to_owned()),
                data: Bytes::from_static(b"png"),
            },
        ])
        .unwrap()
    }

    #[tokio::test]
    async fn managed_edit_for_an_unpriced_model_is_refused_before_billing_or_the_provider() {
        let (provider, aether) = (MockServer::start().await, MockServer::start().await);
        Mock::given(path_regex(r"^/v1/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&provider)
            .await;
        Mock::given(path_regex(r"^/(credits|usage)/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&aether)
            .await;

        let err = server(&provider, &aether)
            .edit(
                &edit_request("openai/dall-e-2"),
                &context("creator", BillingMode::Managed),
            )
            .await
            .unwrap_err();

        assert!(matches!(err, ImageGenError::ModelNotPriced { .. }));
        assert_eq!(err.status_code(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn allowance_product_edit_draws_on_its_meter_never_the_wallet() {
        let (provider, aether) = (MockServer::start().await, MockServer::start().await);
        Mock::given(method("POST"))
            .and(path("/v1/images/edits"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "created": 1, "data": [{ "b64_json": "aW1n" }]
            })))
            .expect(1)
            .mount(&provider)
            .await;
        forbid_wallet(&aether).await;
        Mock::given(method("GET"))
            .and(path("/usage/myfi/organization/org_1/ai_credits/check"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "allowed": true })))
            .expect(1)
            .mount(&aether)
            .await;
        Mock::given(method("POST"))
            .and(path("/usage/myfi/organization/org_1/ai_credits/record"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "eventId": "e" })))
            .expect(1)
            .mount(&aether)
            .await;

        server(&provider, &aether)
            .edit(&edit_request("openai/gpt-image-1.5"), &myfi_context())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn request_without_a_billing_identity_uses_the_configured_key() {
        let (provider, aether) = (MockServer::start().await, MockServer::start().await);
        Mock::given(method("POST"))
            .and(path("/v1/images/generations"))
            .and(header("authorization", "Bearer provider-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "created": 1, "data": [{ "url": "https://img.test/1.png" }]
            })))
            .expect(1)
            .mount(&provider)
            .await;

        server(&provider, &aether)
            .generate(&request(), &RequestContext::empty())
            .await
            .unwrap();
    }
}
