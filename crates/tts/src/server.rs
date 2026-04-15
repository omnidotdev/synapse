use secrecy::SecretString;
use synapse_config::{FailMode, TtsProviderConfig, TtsProviderType};

use crate::{
    error::TtsError,
    provider::{TtsProvider, elevenlabs::ElevenLabsProvider, openai_tts::OpenAiTtsProvider},
    request::RequestContext,
    types::{SpeechRequest, SpeechResponse},
};

/// TTS server that routes requests to the appropriate provider
pub struct Server {
    providers: Vec<Box<dyn TtsProvider>>,
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

    /// Synthesize text to speech using the appropriate provider
    ///
    /// Routes to a provider based on the model name in the request.
    /// Model format: "provider/model" (e.g. "openai/tts-1" or "elevenlabs/`eleven_multilingual_v2`").
    /// If no provider prefix, uses the first configured provider.
    pub async fn synthesize(
        &self,
        request: SpeechRequest,
        context: &RequestContext,
    ) -> crate::error::Result<SpeechResponse> {
        // Extract provider name from model (format: "provider/model")
        let (provider_name, _model_name) = request.model.split_once('/').unwrap_or(("", &request.model));

        let provider = if provider_name.is_empty() {
            // If no provider prefix, use the first provider
            self.providers
                .first()
                .ok_or_else(|| TtsError::ProviderNotFound("No TTS providers configured".to_string()))?
        } else {
            self.providers
                .iter()
                .find(|p| p.name() == provider_name)
                .ok_or_else(|| TtsError::ProviderNotFound(provider_name.to_string()))?
        };

        // Pre-request credit check for managed billing
        #[cfg(feature = "billing")]
        {
            // Estimate cost based on character count (~$0.015/1K chars for TTS)
            let char_count = request.input.len();
            #[allow(clippy::cast_precision_loss)]
            let estimated_cost = char_count as f64 * 0.000_015;
            if estimated_cost > 0.0 {
                self.check_credits(context, estimated_cost).await?;
            }
        }

        // Capture billing metadata before request is consumed
        #[cfg(feature = "billing")]
        let billing_meta = {
            let model_id = request.model.clone();
            #[allow(clippy::cast_possible_truncation)]
            let char_count = request.input.len() as u32;
            let pname = if provider_name.is_empty() {
                provider.name().to_owned()
            } else {
                provider_name.to_owned()
            };
            (pname, model_id, char_count)
        };

        let response = provider.synthesize(request, context).await?;

        // Post-request billing: record usage and deduct credits
        #[cfg(feature = "billing")]
        {
            let (ref pname, ref model_id, char_count) = billing_meta;
            let request_id = uuid::Uuid::new_v4().to_string();
            self.record_usage(context, pname, model_id, char_count, &request_id);
            self.deduct_credits(context, pname, model_id, char_count, &request_id).await;
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
                    return Err(TtsError::InvalidRequest(format!(
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
                        "credit check failed, allowing TTS request (fail-open mode)"
                    );
                    Ok(())
                }
                FailMode::Closed => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "credit check failed, rejecting TTS request (fail-closed mode)"
                    );
                    Err(TtsError::BillingUnavailable)
                }
            },
        }
    }

    /// Record usage for billing metering
    #[cfg(feature = "billing")]
    fn record_usage(&self, context: &RequestContext, provider_name: &str, model_id: &str, char_count: u32, request_id: &str) {
        let Some(ref recorder) = self.usage_recorder else {
            return;
        };

        let Some(ref identity) = context.billing_identity else {
            return;
        };

        let is_byok = identity.mode == synapse_core::BillingMode::Byok;

        // Use input_tokens to track character count for TTS
        // For BYOK, zero out tokens; only the request count is metered
        recorder.record(synapse_billing::UsageEvent {
            entity_type: identity.entity_type.clone(),
            entity_id: identity.entity_id.clone(),
            model: model_id.to_owned(),
            provider: provider_name.to_owned(),
            input_tokens: if is_byok { 0 } else { char_count },
            output_tokens: 0,
            estimated_cost_usd: 0.0,
            idempotency_key: request_id.to_owned(),
        });
    }

    /// Deduct credits after a successful synthesis
    #[cfg(feature = "billing")]
    async fn deduct_credits(&self, context: &RequestContext, provider_name: &str, model_id: &str, char_count: u32, request_id: &str) {
        let Some(ref client) = self.billing_client else {
            return;
        };

        let Some(ref identity) = context.billing_identity else {
            return;
        };

        if identity.mode != synapse_core::BillingMode::Managed {
            return;
        }

        // Cost based on character count (~$0.015/1K chars)
        let actual_cost = f64::from(char_count) * 0.000_015;

        if actual_cost <= 0.0 {
            return;
        }

        let request = synapse_billing::CreditDeductRequest {
            amount: actual_cost,
            description: Some(format!("{provider_name}/{model_id}")),
            idempotency_key: Some(request_id.to_owned()),
            reference_type: Some("speech".to_owned()),
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
                "failed to deduct credits after speech synthesis"
            );
        }
    }
}

/// Builder for constructing the TTS server from configuration
pub struct TtsServerBuilder<'a> {
    config: &'a synapse_config::Config,
}

impl<'a> TtsServerBuilder<'a> {
    pub const fn new(config: &'a synapse_config::Config) -> Self {
        Self { config }
    }

    pub fn build(self) -> crate::error::Result<Server> {
        let mut providers: Vec<Box<dyn TtsProvider>> = Vec::new();

        for (name, provider_config) in &self.config.tts.providers {
            tracing::debug!("Initializing TTS provider: {name}");

            let provider: Box<dyn TtsProvider> = match &provider_config.provider_type {
                TtsProviderType::OpenaiTts => {
                    let api_key = resolve_api_key(name, provider_config)?;

                    Box::new(OpenAiTtsProvider::new(
                        name.clone(),
                        api_key,
                        provider_config.base_url.clone(),
                    ))
                }
                TtsProviderType::Elevenlabs => {
                    let api_key = resolve_api_key(name, provider_config)?;

                    Box::new(ElevenLabsProvider::new(
                        name.clone(),
                        api_key,
                        provider_config.base_url.clone(),
                    ))
                }
            };

            providers.push(provider);
        }

        if providers.is_empty() {
            tracing::debug!("No TTS providers configured");
        } else {
            tracing::debug!("TTS server initialized with {} provider(s)", providers.len());
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

fn resolve_api_key(name: &str, config: &TtsProviderConfig) -> crate::error::Result<SecretString> {
    config
        .api_key
        .clone()
        .ok_or_else(|| TtsError::ConfigError(format!("API key required for TTS provider '{name}'")))
}
