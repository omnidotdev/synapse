use secrecy::SecretString;
use synapse_config::{EmbeddingsProviderConfig, EmbeddingsProviderType, FailMode};
use synapse_core::RequestContext;

use crate::{
    error::EmbeddingsError,
    provider::{EmbeddingsProvider, openai::OpenAiEmbeddingsProvider},
    types::{EmbeddingRequest, EmbeddingResponse},
};

/// Embeddings server that routes requests to the appropriate provider
pub struct Server {
    providers: Vec<Box<dyn EmbeddingsProvider>>,
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

    /// Generate embeddings using the appropriate provider
    ///
    /// Routes to a provider based on the model name in the request.
    /// Model format: "provider/model" (e.g. "openai/text-embedding-3-small").
    /// If no provider prefix, uses the first configured provider.
    pub async fn embed(
        &self,
        request: &EmbeddingRequest,
        context: &RequestContext,
    ) -> crate::error::Result<EmbeddingResponse> {
        // Extract provider name from model (format: "provider/model")
        let (provider_name, _model_name) = request.model.split_once('/').unwrap_or(("", &request.model));

        let provider = if provider_name.is_empty() {
            // If no provider prefix, use the first provider
            self.providers
                .first()
                .ok_or_else(|| EmbeddingsError::ProviderNotFound("No embeddings providers configured".to_string()))?
        } else {
            self.providers
                .iter()
                .find(|p| p.name() == provider_name)
                .ok_or_else(|| EmbeddingsError::ProviderNotFound(provider_name.to_string()))?
        };

        // Pre-request credit check for managed billing
        #[cfg(feature = "billing")]
        {
            // Estimate tokens from input text length (~4 chars per token)
            let estimated_tokens: usize = request.input.as_vec().iter().map(|s| s.len() / 4).sum();
            #[allow(clippy::cast_precision_loss)]
            let estimated_cost = estimated_tokens as f64 * 0.000_000_02; // ~$0.02/1M tokens
            if estimated_cost > 0.0 {
                self.check_credits(context, estimated_cost).await?;
            }
        }

        let response = provider.embed(request, context).await?;

        // Post-request billing: record usage and deduct credits
        #[cfg(feature = "billing")]
        {
            let pname = if provider_name.is_empty() {
                provider.name()
            } else {
                provider_name
            };
            self.record_usage(context, pname, &request.model, response.usage.prompt_tokens);
            self.deduct_credits(context, pname, &request.model, response.usage.prompt_tokens)
                .await;
        }

        Ok(response)
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
                    return Err(EmbeddingsError::InvalidRequest(format!(
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
                        "credit check failed, allowing embeddings request (fail-open mode)"
                    );
                    Ok(())
                }
                FailMode::Closed => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "credit check failed, rejecting embeddings request (fail-closed mode)"
                    );
                    Err(EmbeddingsError::BillingUnavailable)
                }
            },
        }
    }

    /// Record usage for billing metering
    #[cfg(feature = "billing")]
    fn record_usage(&self, context: &RequestContext, provider_name: &str, model_id: &str, prompt_tokens: u32) {
        let Some(ref recorder) = self.usage_recorder else {
            return;
        };

        let Some(ref identity) = context.billing_identity else {
            return;
        };

        let is_byok = identity.mode == synapse_core::BillingMode::Byok;
        let idempotency_key = uuid::Uuid::new_v4().to_string();

        // For BYOK, zero out tokens — only the request count is metered
        recorder.record(synapse_billing::UsageEvent {
            entity_type: identity.entity_type.clone(),
            entity_id: identity.entity_id.clone(),
            model: model_id.to_owned(),
            provider: provider_name.to_owned(),
            input_tokens: if is_byok { 0 } else { prompt_tokens },
            output_tokens: 0,
            estimated_cost_usd: 0.0,
            idempotency_key,
        });
    }

    /// Deduct credits after a successful embedding request
    #[cfg(feature = "billing")]
    async fn deduct_credits(&self, context: &RequestContext, provider_name: &str, model_id: &str, prompt_tokens: u32) {
        let Some(ref client) = self.billing_client else {
            return;
        };

        let Some(ref identity) = context.billing_identity else {
            return;
        };

        if identity.mode != synapse_core::BillingMode::Managed {
            return;
        }

        // Cost based on token count (~$0.02/1M tokens for text-embedding-3-small)
        let actual_cost = f64::from(prompt_tokens) * 0.000_000_02;

        if actual_cost <= 0.0 {
            return;
        }

        let request = synapse_billing::CreditDeductRequest {
            amount: actual_cost,
            description: Some(format!("{provider_name}/{model_id}")),
            idempotency_key: Some(uuid::Uuid::new_v4().to_string()),
            reference_type: Some("embedding".to_owned()),
            reference_id: None,
        };

        if let Err(e) = client
            .deduct_credits(&identity.entity_type, &identity.entity_id, &request)
            .await
        {
            tracing::warn!(
                error = %e,
                entity_id = %identity.entity_id,
                cost = actual_cost,
                "failed to deduct credits after embedding"
            );
        }
    }
}

/// Builder for constructing the embeddings server from configuration
pub struct EmbeddingsServerBuilder<'a> {
    config: &'a synapse_config::Config,
}

impl<'a> EmbeddingsServerBuilder<'a> {
    pub fn new(config: &'a synapse_config::Config) -> Self {
        Self { config }
    }

    pub fn build(self) -> crate::error::Result<Server> {
        let mut providers: Vec<Box<dyn EmbeddingsProvider>> = Vec::new();

        for (name, provider_config) in &self.config.embeddings.providers {
            tracing::debug!("Initializing embeddings provider: {name}");

            let provider: Box<dyn EmbeddingsProvider> = match &provider_config.provider_type {
                EmbeddingsProviderType::Openai => {
                    let api_key = resolve_api_key(name, provider_config)?;

                    Box::new(OpenAiEmbeddingsProvider::new(
                        name.clone(),
                        api_key,
                        provider_config.base_url.clone(),
                    ))
                }
            };

            providers.push(provider);
        }

        if providers.is_empty() {
            tracing::debug!("No embeddings providers configured");
        } else {
            tracing::debug!("Embeddings server initialized with {} provider(s)", providers.len());
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

fn resolve_api_key(name: &str, config: &EmbeddingsProviderConfig) -> crate::error::Result<SecretString> {
    config
        .api_key
        .clone()
        .ok_or_else(|| EmbeddingsError::ConfigError(format!("API key required for embeddings provider '{name}'")))
}
