use secrecy::SecretString;
use synapse_config::{EmbeddingsProviderConfig, EmbeddingsProviderType};
use synapse_core::RequestContext;

use crate::{
    error::EmbeddingsError,
    provider::{EmbeddingsProvider, openai::OpenAiEmbeddingsProvider},
    types::{EmbeddingRequest, EmbeddingResponse},
};

/// Embeddings server that routes requests to the appropriate provider
pub struct Server {
    providers: Vec<Box<dyn EmbeddingsProvider>>,
}

impl Server {
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

        provider.embed(request, context).await
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

        Ok(Server { providers })
    }
}

fn resolve_api_key(name: &str, config: &EmbeddingsProviderConfig) -> crate::error::Result<SecretString> {
    config
        .api_key
        .clone()
        .ok_or_else(|| EmbeddingsError::ConfigError(format!("API key required for embeddings provider '{name}'")))
}
