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
    pub fn set_billing_client(&mut self, client: synapse_billing::AetherClient, fail_mode: FailMode) {
        self.billing_client = Some(client);
        self.billing_fail_mode = fail_mode;
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

        // Pre-request credit check for managed billing
        #[cfg(feature = "billing")]
        {
            let estimated_cost = Self::estimate_image_cost(request);
            if estimated_cost > 0.0 {
                self.check_credits(context, estimated_cost).await?;
            }
        }

        let response = provider.generate(request, context).await?;

        // Post-request billing: record usage and deduct credits
        #[cfg(feature = "billing")]
        {
            let pname = if provider_name.is_empty() {
                provider.name()
            } else {
                provider_name
            };
            self.record_usage(context, pname, request);
            self.deduct_credits(context, pname, request).await;
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

    /// Check if the user has sufficient credits for the estimated cost
    #[cfg(feature = "billing")]
    async fn check_credits(&self, context: &RequestContext, estimated_cost: f64) -> crate::error::Result<()> {
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
            .check_credits(&identity.entity_type, &identity.entity_id, estimated_cost)
            .await
        {
            Ok(response) => {
                if !response.sufficient {
                    return Err(ImageGenError::InvalidRequest(format!(
                        "insufficient credits: estimated cost ${estimated_cost:.4} exceeds available balance ${:.4}",
                        response.balance
                    )));
                }
                Ok(())
            }
            Err(e) => match self.billing_fail_mode {
                FailMode::Open => {
                    tracing::warn!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "credit check failed, allowing image generation request (fail-open mode)"
                    );
                    Ok(())
                }
                FailMode::Closed => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "credit check failed, rejecting image generation request (fail-closed mode)"
                    );
                    Err(ImageGenError::BillingUnavailable)
                }
            },
        }
    }

    /// Record usage for billing metering
    #[cfg(feature = "billing")]
    fn record_usage(&self, context: &RequestContext, provider_name: &str, request: &ImageRequest) {
        let Some(ref recorder) = self.usage_recorder else {
            return;
        };

        let Some(ref identity) = context.billing_identity else {
            return;
        };

        let is_byok = identity.mode == synapse_core::BillingMode::Byok;
        let idempotency_key = uuid::Uuid::new_v4().to_string();

        // Use input_tokens to track image count for imagegen
        // For BYOK, zero out tokens and cost — only the request count is metered
        recorder.record(synapse_billing::UsageEvent {
            entity_type: identity.entity_type.clone(),
            entity_id: identity.entity_id.clone(),
            model: request.model.clone(),
            provider: provider_name.to_owned(),
            input_tokens: if is_byok { 0 } else { request.n },
            output_tokens: 0,
            estimated_cost_usd: if is_byok {
                0.0
            } else {
                Self::estimate_image_cost(request)
            },
            idempotency_key,
        });
    }

    /// Deduct credits after successful image generation
    #[cfg(feature = "billing")]
    async fn deduct_credits(&self, context: &RequestContext, provider_name: &str, request: &ImageRequest) {
        let Some(ref client) = self.billing_client else {
            return;
        };

        let Some(ref identity) = context.billing_identity else {
            return;
        };

        if identity.mode != synapse_core::BillingMode::Managed {
            return;
        }

        let actual_cost = Self::estimate_image_cost(request);

        if actual_cost <= 0.0 {
            return;
        }

        let deduct_request = synapse_billing::CreditDeductRequest {
            amount: actual_cost,
            description: Some(format!("{provider_name}/{}", request.model)),
            idempotency_key: Some(uuid::Uuid::new_v4().to_string()),
            reference_type: Some("image_generation".to_owned()),
            reference_id: None,
        };

        if let Err(e) = client
            .deduct_credits(&identity.entity_type, &identity.entity_id, &deduct_request)
            .await
        {
            tracing::warn!(
                error = %e,
                entity_id = %identity.entity_id,
                cost = actual_cost,
                "failed to deduct credits after image generation"
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
        })
    }
}

fn resolve_api_key(name: &str, config: &ImageGenProviderConfig) -> crate::error::Result<SecretString> {
    config
        .api_key
        .clone()
        .ok_or_else(|| ImageGenError::ConfigError(format!("API key required for image generation provider '{name}'")))
}
