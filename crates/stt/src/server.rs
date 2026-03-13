use secrecy::SecretString;
use synapse_config::{SttProviderConfig, SttProviderType};

use crate::{
    error::SttError,
    provider::{SttProvider, deepgram::DeepgramProvider, whisper::WhisperProvider},
    request::RequestContext,
    types::{TranscriptionRequest, TranscriptionResponse},
};

/// STT server that routes requests to the appropriate provider
pub struct Server {
    providers: Vec<Box<dyn SttProvider>>,
    /// Usage recorder for billing metering
    #[cfg(feature = "billing")]
    usage_recorder: Option<synapse_billing::UsageRecorder>,
    /// Billing client for pre-request credit checks
    #[cfg(feature = "billing")]
    billing_client: Option<synapse_billing::AetherClient>,
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
    pub fn set_billing_client(&mut self, client: synapse_billing::AetherClient) {
        self.billing_client = Some(client);
    }

    /// Transcribe audio using the appropriate provider
    ///
    /// Routes to a provider based on the model name in the request.
    /// Model format: "provider/model" (e.g. "openai/whisper-1" or "deepgram/nova-2").
    /// If no provider prefix, uses the first configured provider.
    pub async fn transcribe(
        &self,
        request: TranscriptionRequest,
        context: &RequestContext,
    ) -> crate::error::Result<TranscriptionResponse> {
        // Extract provider name from model (format: "provider/model")
        let (provider_name, _model_name) = request.model.split_once('/').unwrap_or(("", &request.model));

        let provider = if provider_name.is_empty() {
            // If no provider prefix, use the first provider
            self.providers
                .first()
                .ok_or_else(|| SttError::ProviderNotFound("No STT providers configured".to_string()))?
        } else {
            self.providers
                .iter()
                .find(|p| p.name() == provider_name)
                .ok_or_else(|| SttError::ProviderNotFound(provider_name.to_string()))?
        };

        // Pre-request credit check for managed billing
        #[cfg(feature = "billing")]
        {
            // Estimate cost based on audio size (rough proxy: 1 second ~= 16KB at 16kHz 16-bit mono)
            #[allow(clippy::cast_precision_loss)]
            let estimated_duration_secs = request.audio.len() as f64 / 16_000.0;
            let estimated_cost = estimated_duration_secs * 0.0001; // ~$0.006/min
            if estimated_cost > 0.0 {
                self.check_credits(context, estimated_cost).await?;
            }
        }

        // Capture billing metadata before request is consumed
        #[cfg(feature = "billing")]
        let billing_meta = {
            let model_id = request.model.clone();
            #[allow(clippy::cast_possible_truncation)]
            let audio_bytes = request.audio.len() as u32;
            let pname = if provider_name.is_empty() {
                provider.name().to_owned()
            } else {
                provider_name.to_owned()
            };
            (pname, model_id, audio_bytes)
        };

        let response = provider.transcribe(request, context).await?;

        // Post-request billing: record usage and deduct credits
        #[cfg(feature = "billing")]
        {
            let (ref pname, ref model_id, audio_bytes) = billing_meta;
            self.record_usage(context, pname, model_id, audio_bytes);
            self.deduct_credits(context, pname, model_id, audio_bytes).await;
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
                    return Err(SttError::InvalidRequest(format!(
                        "insufficient credits: estimated cost ${estimated_cost:.4} exceeds available balance ${:.4}",
                        response.balance
                    )));
                }
                Ok(())
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    entity_id = %identity.entity_id,
                    "credit check failed, allowing STT request"
                );
                Ok(())
            }
        }
    }

    /// Record usage for billing metering
    #[cfg(feature = "billing")]
    fn record_usage(&self, context: &RequestContext, provider_name: &str, model_id: &str, audio_bytes: u32) {
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
            input_tokens: if is_byok { 0 } else { audio_bytes },
            output_tokens: 0,
            estimated_cost_usd: 0.0,
            idempotency_key,
        });
    }

    /// Deduct credits after a successful transcription
    #[cfg(feature = "billing")]
    async fn deduct_credits(&self, context: &RequestContext, provider_name: &str, model_id: &str, audio_bytes: u32) {
        let Some(ref client) = self.billing_client else {
            return;
        };

        let Some(ref identity) = context.billing_identity else {
            return;
        };

        if identity.mode != synapse_core::BillingMode::Managed {
            return;
        }

        // Cost based on audio duration (~$0.006/min whisper pricing)
        let duration_secs = f64::from(audio_bytes) / 16_000.0;
        let actual_cost = duration_secs * 0.0001;

        if actual_cost <= 0.0 {
            return;
        }

        let request = synapse_billing::CreditDeductRequest {
            amount: actual_cost,
            description: Some(format!("{provider_name}/{model_id}")),
            idempotency_key: Some(uuid::Uuid::new_v4().to_string()),
            reference_type: Some("transcription".to_owned()),
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
                "failed to deduct credits after transcription"
            );
        }
    }
}

/// Builder for constructing the STT server from configuration
pub struct SttServerBuilder<'a> {
    config: &'a synapse_config::Config,
}

impl<'a> SttServerBuilder<'a> {
    pub fn new(config: &'a synapse_config::Config) -> Self {
        Self { config }
    }

    pub fn build(self) -> crate::error::Result<Server> {
        let mut providers: Vec<Box<dyn SttProvider>> = Vec::new();

        for (name, provider_config) in &self.config.stt.providers {
            tracing::debug!("Initializing STT provider: {name}");

            let provider: Box<dyn SttProvider> = match &provider_config.provider_type {
                SttProviderType::Whisper => {
                    let api_key = resolve_api_key(name, provider_config)?;

                    Box::new(WhisperProvider::new(
                        name.clone(),
                        api_key,
                        provider_config.base_url.clone(),
                    ))
                }
                SttProviderType::Deepgram => {
                    let api_key = resolve_api_key(name, provider_config)?;

                    Box::new(DeepgramProvider::new(
                        name.clone(),
                        api_key,
                        provider_config.base_url.clone(),
                    ))
                }
            };

            providers.push(provider);
        }

        if providers.is_empty() {
            tracing::debug!("No STT providers configured");
        } else {
            tracing::debug!("STT server initialized with {} provider(s)", providers.len());
        }

        Ok(Server {
            providers,
            #[cfg(feature = "billing")]
            usage_recorder: None,
            #[cfg(feature = "billing")]
            billing_client: None,
        })
    }
}

fn resolve_api_key(name: &str, config: &SttProviderConfig) -> crate::error::Result<SecretString> {
    config
        .api_key
        .clone()
        .ok_or_else(|| SttError::ConfigError(format!("API key required for STT provider '{name}'")))
}
