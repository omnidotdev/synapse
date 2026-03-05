use async_trait::async_trait;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use synapse_core::RequestContext;

use super::ImageGenProvider;
use crate::{
    error::{ImageGenError, Result},
    types::{ImageData, ImageRequest, ImageResponse},
};

/// Default `OpenAI` API base URL
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// `OpenAI` image generation provider
pub struct OpenAiImageGenProvider {
    name: String,
    client: Client,
    api_key: SecretString,
    base_url: String,
}

impl OpenAiImageGenProvider {
    /// Create a new `OpenAI` image generation provider
    pub fn new(name: String, api_key: SecretString, base_url: Option<String>) -> Self {
        let base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        Self {
            name,
            client: Client::new(),
            api_key,
            base_url,
        }
    }

    /// Strip the "provider/" prefix from a model name
    ///
    /// Model names arrive as "openai/dall-e-3"; the upstream
    /// API expects just "dall-e-3"
    fn strip_model_prefix(model: &str) -> &str {
        model.split_once('/').map_or(model, |(_, model_name)| model_name)
    }
}

/// Wire format for the `OpenAI` image generation API request
#[derive(Serialize)]
struct OpenAiImageRequest {
    prompt: String,
    model: String,
    n: u32,
    size: String,
    quality: String,
    response_format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
}

/// Wire format for the `OpenAI` image generation API response
#[derive(Deserialize)]
struct OpenAiImageResponse {
    created: u64,
    data: Vec<OpenAiImageData>,
}

#[derive(Deserialize)]
struct OpenAiImageData {
    url: Option<String>,
    b64_json: Option<String>,
    revised_prompt: Option<String>,
}

#[async_trait]
impl ImageGenProvider for OpenAiImageGenProvider {
    async fn generate(&self, request: &ImageRequest, _context: &RequestContext) -> Result<ImageResponse> {
        let url = format!("{}/images/generations", self.base_url.trim_end_matches('/'));
        let model = Self::strip_model_prefix(&request.model).to_string();

        let wire_request = OpenAiImageRequest {
            prompt: request.prompt.clone(),
            model,
            n: request.n,
            size: request.size.clone(),
            quality: request.quality.clone(),
            response_format: request.response_format.clone(),
            user: request.user.clone(),
        };

        tracing::debug!(
            provider = %self.name,
            model = %request.model,
            "sending image generation request"
        );

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key.expose_secret()))
            .json(&wire_request)
            .send()
            .await
            .map_err(|e| {
                tracing::error!(provider = %self.name, error = %e, "image generation request failed");
                ImageGenError::ConnectionError(format!("Failed to send request to OpenAI image generation: {e}"))
            })?;

        let status = response.status();

        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());

            tracing::error!(
                provider = %self.name,
                status = %status,
                "OpenAI image generation API error"
            );

            return Err(match status.as_u16() {
                401 => ImageGenError::AuthenticationFailed(error_text),
                400 => ImageGenError::InvalidRequest(error_text),
                _ => ImageGenError::ProviderApiError {
                    status: status.as_u16(),
                    message: error_text,
                },
            });
        }

        let wire_response: OpenAiImageResponse = response.json().await.map_err(|e| {
            tracing::error!(
                provider = %self.name,
                error = %e,
                "failed to parse OpenAI image generation response"
            );
            ImageGenError::InternalError(None)
        })?;

        tracing::debug!(provider = %self.name, "image generation request complete");

        Ok(ImageResponse {
            created: wire_response.created,
            data: wire_response
                .data
                .into_iter()
                .map(|d| ImageData {
                    url: d.url,
                    b64_json: d.b64_json,
                    revised_prompt: d.revised_prompt,
                })
                .collect(),
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}
