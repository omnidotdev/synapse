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
}

impl Server {
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

        provider.transcribe(request, context).await
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

        Ok(Server { providers })
    }
}

fn resolve_api_key(name: &str, config: &SttProviderConfig) -> crate::error::Result<SecretString> {
    config
        .api_key
        .clone()
        .ok_or_else(|| SttError::ConfigError(format!("API key required for STT provider '{name}'")))
}
