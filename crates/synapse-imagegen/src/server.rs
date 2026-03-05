use secrecy::SecretString;
use synapse_config::{ImageGenProviderConfig, ImageGenProviderType};
use synapse_core::RequestContext;

use crate::{
    error::ImageGenError,
    provider::{ImageGenProvider, openai::OpenAiImageGenProvider},
    types::{ImageRequest, ImageResponse},
};

/// Image generation server that routes requests to the appropriate provider
pub struct Server {
    providers: Vec<Box<dyn ImageGenProvider>>,
}

impl Server {
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

        provider.generate(request, context).await
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

        Ok(Server { providers })
    }
}

fn resolve_api_key(name: &str, config: &ImageGenProviderConfig) -> crate::error::Result<SecretString> {
    config
        .api_key
        .clone()
        .ok_or_else(|| ImageGenError::ConfigError(format!("API key required for image generation provider '{name}'")))
}
