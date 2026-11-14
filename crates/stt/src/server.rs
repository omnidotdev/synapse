use secrecy::SecretString;
use synapse_config::{FailMode, SttProviderConfig, SttProviderType};

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

        // Pre-request spend check for managed billing
        #[cfg(feature = "billing")]
        {
            // Estimate cost based on audio size (rough proxy: 1 second ~= 16KB at 16kHz 16-bit mono)
            #[allow(clippy::cast_precision_loss)]
            let estimated_duration_secs = request.audio.len() as f64 / 16_000.0;
            let estimated_cost = self.charge(context, provider.name(), estimated_duration_secs * 0.0001); // ~$0.006/min
            if estimated_cost > 0.0 {
                self.check_spend(context, estimated_cost).await?;
            }
        }

        // Capture billing metadata before request is consumed
        #[cfg(feature = "billing")]
        let billing_meta = {
            let model_id = request.model.clone();
            let audio_bytes: u32 = request.audio.len().try_into().unwrap_or(u32::MAX);
            let pname = if provider_name.is_empty() {
                provider.name().to_owned()
            } else {
                provider_name.to_owned()
            };
            (pname, model_id, audio_bytes)
        };

        let response = provider.transcribe(request, context).await?;

        // Post-request billing: record usage and charge the actual cost
        #[cfg(feature = "billing")]
        {
            let (ref pname, ref model_id, audio_bytes) = billing_meta;
            let request_id = uuid::Uuid::new_v4().to_string();
            self.record_usage(context, pname, model_id, audio_bytes, &request_id);
            self.settle_spend(context, pname, model_id, audio_bytes, &request_id)
                .await;
        }

        Ok(response)
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
            Ok(synapse_billing::SpendCheck::InsufficientCredits { balance }) => Err(SttError::InvalidRequest(format!(
                "insufficient credits: estimated cost ${estimated_cost:.4} exceeds available balance ${balance:.4}"
            ))),
            Ok(synapse_billing::SpendCheck::AllowanceExhausted) => Err(SttError::AllowanceExhausted),
            Err(e) => match self.billing_fail_mode {
                FailMode::Degraded => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, allowing STT request (degraded mode)"
                    );
                    Ok(())
                }
                FailMode::Open => {
                    tracing::warn!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, allowing STT request (fail-open mode)"
                    );
                    Ok(())
                }
                FailMode::Closed => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, rejecting STT request (fail-closed mode)"
                    );
                    Err(SttError::BillingUnavailable)
                }
            },
        }
    }

    /// Record usage for billing metering
    #[cfg(feature = "billing")]
    fn record_usage(
        &self,
        context: &RequestContext,
        provider_name: &str,
        model_id: &str,
        audio_bytes: u32,
        request_id: &str,
    ) {
        let Some(ref recorder) = self.usage_recorder else {
            return;
        };

        let Some(ref identity) = context.billing_identity else {
            return;
        };

        let is_byok = identity.mode == synapse_core::BillingMode::Byok;

        // For BYOK, zero out tokens; only the request count is metered
        recorder.record(synapse_billing::UsageEvent {
            app_id: identity.app_id.clone(),
            entity_type: identity.entity_type.clone(),
            entity_id: identity.entity_id.clone(),
            model: model_id.to_owned(),
            provider: provider_name.to_owned(),
            input_tokens: if is_byok { 0 } else { audio_bytes },
            output_tokens: 0,
            estimated_cost_usd: 0.0,
            idempotency_key: request_id.to_owned(),
        });
    }

    /// Charge the actual cost after a successful transcription to the credit wallet, or to the
    /// product's included allowance
    #[cfg(feature = "billing")]
    async fn settle_spend(
        &self,
        context: &RequestContext,
        provider_name: &str,
        model_id: &str,
        audio_bytes: u32,
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

        // Cost based on audio duration (~$0.006/min whisper pricing)
        let duration_secs = f64::from(audio_bytes) / 16_000.0;
        let actual_cost = self.charge(context, provider_name, duration_secs * 0.0001);

        if actual_cost <= 0.0 {
            return;
        }

        let charge = synapse_billing::SpendCharge {
            amount_usd: actual_cost,
            description: format!("{provider_name}/{model_id}"),
            idempotency_key: request_id.to_owned(),
            reference_type: "transcription".to_owned(),
        };

        if let Err(e) = client
            .settle_spend(&identity.app_id, &identity.entity_type, &identity.entity_id, &charge)
            .await
        {
            tracing::warn!(
                error = %e,
                entity_id = %identity.entity_id,
                cost = actual_cost,
                "failed to charge usage after transcription"
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
            #[cfg(feature = "billing")]
            billing_fail_mode: FailMode::default(),
            #[cfg(feature = "billing")]
            margins: synapse_core::MarginPolicy::default(),
        })
    }
}

fn resolve_api_key(name: &str, config: &SttProviderConfig) -> crate::error::Result<SecretString> {
    config
        .api_key
        .clone()
        .ok_or_else(|| SttError::ConfigError(format!("API key required for STT provider '{name}'")))
}
