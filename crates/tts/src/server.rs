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

        // A BYOK request is not metered, so it must run on the caller's own
        // key, never the configured one
        let api_key = synapse_core::byok_provider_key(
            context.billing_identity.as_ref(),
            &context.provider_keys,
            provider.key_provider(),
        )
        .map_err(|e| TtsError::InvalidRequest(e.to_string()))?;

        // Pre-request spend check for managed billing
        #[cfg(feature = "billing")]
        {
            // Estimate cost based on character count (~$0.015/1K chars for TTS)
            let char_count = request.input.len();
            #[allow(clippy::cast_precision_loss)]
            let estimated_cost = self.charge(context, provider.name(), char_count as f64 * 0.000_015);
            if estimated_cost > 0.0 {
                self.check_spend(context, estimated_cost).await?;
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

        let response = provider.synthesize(request, context, api_key).await?;

        // Post-request billing: record usage and charge the actual cost
        #[cfg(feature = "billing")]
        {
            let (ref pname, ref model_id, char_count) = billing_meta;
            let request_id = uuid::Uuid::new_v4().to_string();
            self.record_usage(context, pname, model_id, char_count, &request_id);
            self.settle_spend(context, pname, model_id, char_count, &request_id)
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
            Ok(synapse_billing::SpendCheck::InsufficientCredits { balance }) => Err(TtsError::InvalidRequest(format!(
                "insufficient credits: estimated cost ${estimated_cost:.4} exceeds available balance ${balance:.4}"
            ))),
            Ok(synapse_billing::SpendCheck::AllowanceExhausted) => Err(TtsError::AllowanceExhausted),
            Err(e) => match self.billing_fail_mode {
                FailMode::Degraded => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, allowing TTS request (degraded mode)"
                    );
                    Ok(())
                }
                FailMode::Open => {
                    tracing::warn!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, allowing TTS request (fail-open mode)"
                    );
                    Ok(())
                }
                FailMode::Closed => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, rejecting TTS request (fail-closed mode)"
                    );
                    Err(TtsError::BillingUnavailable)
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
        char_count: u32,
        request_id: &str,
    ) {
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
            app_id: identity.app_id.clone(),
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

    /// Charge the actual cost after a successful synthesis to the credit wallet, or to the
    /// product's included allowance
    #[cfg(feature = "billing")]
    async fn settle_spend(
        &self,
        context: &RequestContext,
        provider_name: &str,
        model_id: &str,
        char_count: u32,
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

        // Cost based on character count (~$0.015/1K chars)
        let actual_cost = self.charge(context, provider_name, f64::from(char_count) * 0.000_015);

        if actual_cost <= 0.0 {
            return;
        }

        let charge = synapse_billing::SpendCharge {
            amount_usd: actual_cost,
            description: format!("{provider_name}/{model_id}"),
            idempotency_key: request_id.to_owned(),
            reference_type: "speech".to_owned(),
        };

        if let Err(e) = client
            .settle_spend(&identity.app_id, &identity.entity_type, &identity.entity_id, &charge)
            .await
        {
            tracing::warn!(
                error = %e,
                entity_id = %identity.entity_id,
                cost = actual_cost,
                "failed to charge usage after speech synthesis"
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
            #[cfg(feature = "billing")]
            margins: synapse_core::MarginPolicy::default(),
        })
    }
}

fn resolve_api_key(name: &str, config: &TtsProviderConfig) -> crate::error::Result<SecretString> {
    config
        .api_key
        .clone()
        .ok_or_else(|| TtsError::ConfigError(format!("API key required for TTS provider '{name}'")))
}

#[cfg(test)]
mod key_tests {
    use secrecy::SecretString;
    use synapse_core::{BillingIdentity, BillingMode};
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::Server;
    use crate::error::TtsError;
    use crate::provider::{TtsProvider, elevenlabs::ElevenLabsProvider, openai_tts::OpenAiTtsProvider};
    use crate::request::RequestContext;
    use crate::types::SpeechRequest;

    fn server(provider: Box<dyn TtsProvider>) -> Server {
        Server {
            providers: vec![provider],
            #[cfg(feature = "billing")]
            usage_recorder: None,
            #[cfg(feature = "billing")]
            billing_client: None,
            #[cfg(feature = "billing")]
            billing_fail_mode: synapse_config::FailMode::default(),
            #[cfg(feature = "billing")]
            margins: synapse_core::MarginPolicy::default(),
        }
    }

    fn openai(upstream: &MockServer) -> Server {
        server(Box::new(OpenAiTtsProvider::new(
            "openai".to_owned(),
            SecretString::from("configured-key".to_owned()),
            Some(format!("{}/v1", upstream.uri())),
        )))
    }

    fn elevenlabs(upstream: &MockServer) -> Server {
        server(Box::new(ElevenLabsProvider::new(
            "elevenlabs".to_owned(),
            SecretString::from("configured-key".to_owned()),
            Some(upstream.uri()),
        )))
    }

    async fn mount_audio(upstream: &MockServer) {
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "audio/mpeg")
                    .set_body_bytes(vec![1, 2, 3]),
            )
            .mount(upstream)
            .await;
    }

    fn request(model: &str) -> SpeechRequest {
        SpeechRequest {
            model: model.to_owned(),
            input: "hello".to_owned(),
            voice: "alloy".to_owned(),
            response_format: None,
            speed: None,
        }
    }

    fn context(mode: Option<BillingMode>, keys: &[(&str, &str)]) -> RequestContext {
        let (parts, ()) = http::Request::new(()).into_parts();
        RequestContext {
            parts,
            // A client-supplied header key is never used upstream
            api_key: Some(SecretString::from("client-header-key")),
            client_identity: None,
            authentication: synapse_core::Authentication::default(),
            billing_identity: mode.map(|mode| BillingIdentity {
                app_id: "synapse".to_owned(),
                entity_type: "user".to_owned(),
                entity_id: "usr_1".to_owned(),
                mode,
                plan: None,
            }),
            provider_keys: keys
                .iter()
                .map(|(name, key)| ((*name).to_owned(), SecretString::from(*key)))
                .collect(),
        }
    }

    async fn header_sent(upstream: &MockServer, header: &str) -> Vec<String> {
        upstream
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r.headers.get(header).and_then(|v| v.to_str().ok()).map(str::to_owned))
            .collect()
    }

    #[tokio::test]
    async fn byok_request_runs_on_the_callers_openai_key() {
        let upstream = MockServer::start().await;
        mount_audio(&upstream).await;

        openai(&upstream)
            .synthesize(
                request("openai/tts-1"),
                &context(Some(BillingMode::Byok), &[("openai", "byok-openai")]),
            )
            .await
            .unwrap();

        assert_eq!(
            header_sent(&upstream, "authorization").await,
            vec!["Bearer byok-openai"]
        );
    }

    #[tokio::test]
    async fn byok_request_on_a_custom_named_provider_runs_on_the_vendor_key() {
        // A provider configured under its own name (`[tts.providers.voice]`,
        // type `openai_tts`) still runs on the caller's `openai` key
        let upstream = MockServer::start().await;
        mount_audio(&upstream).await;

        server(Box::new(OpenAiTtsProvider::new(
            "voice".to_owned(),
            SecretString::from("configured-key".to_owned()),
            Some(format!("{}/v1", upstream.uri())),
        )))
        .synthesize(
            request("voice/tts-1"),
            &context(Some(BillingMode::Byok), &[("openai", "byok-openai")]),
        )
        .await
        .unwrap();

        assert_eq!(
            header_sent(&upstream, "authorization").await,
            vec!["Bearer byok-openai"]
        );
    }

    #[tokio::test]
    async fn byok_request_runs_on_the_callers_elevenlabs_key() {
        let upstream = MockServer::start().await;
        mount_audio(&upstream).await;

        elevenlabs(&upstream)
            .synthesize(
                request("elevenlabs/eleven_multilingual_v2"),
                &context(Some(BillingMode::Byok), &[("elevenlabs", "byok-elevenlabs")]),
            )
            .await
            .unwrap();

        assert_eq!(header_sent(&upstream, "xi-api-key").await, vec!["byok-elevenlabs"]);
    }

    #[tokio::test]
    async fn byok_request_without_a_key_is_a_400_before_any_upstream_call() {
        let upstream = MockServer::start().await;
        mount_audio(&upstream).await;

        let Err(err) = openai(&upstream)
            .synthesize(
                request("openai/tts-1"),
                &context(Some(BillingMode::Byok), &[("elevenlabs", "byok-other")]),
            )
            .await
        else {
            panic!("a BYOK request without a key must be refused");
        };

        assert!(matches!(err, TtsError::InvalidRequest(_)), "{err:?}");
        assert_eq!(err.status_code(), axum::http::StatusCode::BAD_REQUEST);
        assert!(upstream.received_requests().await.unwrap_or_default().is_empty());
    }

    #[tokio::test]
    async fn managed_request_runs_on_the_configured_key() {
        let upstream = MockServer::start().await;
        mount_audio(&upstream).await;

        openai(&upstream)
            .synthesize(
                request("openai/tts-1"),
                &context(Some(BillingMode::Managed), &[("openai", "byok-ignored")]),
            )
            .await
            .unwrap();

        assert_eq!(
            header_sent(&upstream, "authorization").await,
            vec!["Bearer configured-key"]
        );
    }

    #[tokio::test]
    async fn request_without_a_billing_identity_runs_on_the_configured_key() {
        let upstream = MockServer::start().await;
        mount_audio(&upstream).await;

        openai(&upstream)
            .synthesize(request("openai/tts-1"), &context(None, &[]))
            .await
            .unwrap();

        assert_eq!(
            header_sent(&upstream, "authorization").await,
            vec!["Bearer configured-key"]
        );
    }
}
