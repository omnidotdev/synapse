use secrecy::SecretString;
use synapse_config::{TtsProviderConfig, TtsProviderType};

use crate::{
    error::TtsError,
    provider::{TtsProvider, elevenlabs::ElevenLabsProvider, openai_tts::OpenAiTtsProvider},
    request::RequestContext,
    types::{SpeechRequest, SpeechResponse},
};

/// TTS server that routes requests to the appropriate provider
pub struct Server {
    providers: Vec<Box<dyn TtsProvider>>,
}

impl Server {
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

        provider.synthesize(request, context).await
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

        Ok(Server { providers })
    }
}

fn resolve_api_key(name: &str, config: &TtsProviderConfig) -> crate::error::Result<SecretString> {
    config
        .api_key
        .clone()
        .ok_or_else(|| TtsError::ConfigError(format!("API key required for TTS provider '{name}'")))
}
