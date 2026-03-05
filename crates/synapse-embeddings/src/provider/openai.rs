use async_trait::async_trait;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use synapse_core::RequestContext;

use super::EmbeddingsProvider;
use crate::{
    error::{EmbeddingsError, Result},
    types::{EmbeddingRequest, EmbeddingResponse},
};

/// Default `OpenAI` API base URL
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// `OpenAI` embeddings provider
pub struct OpenAiEmbeddingsProvider {
    name: String,
    client: Client,
    api_key: SecretString,
    base_url: String,
}

impl OpenAiEmbeddingsProvider {
    /// Create a new `OpenAI` embeddings provider
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
    /// Model names arrive as "openai/text-embedding-3-small"; the upstream
    /// API expects just "text-embedding-3-small"
    fn strip_model_prefix(model: &str) -> &str {
        model.split_once('/').map_or(model, |(_, model_name)| model_name)
    }
}

/// Wire format for the `OpenAI` embeddings API request
#[derive(Serialize)]
struct OpenAiEmbeddingRequest {
    input: Vec<String>,
    model: String,
    encoding_format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<u32>,
}

/// Wire format for the `OpenAI` embeddings API response
#[derive(Deserialize)]
struct OpenAiEmbeddingResponse {
    object: String,
    data: Vec<OpenAiEmbeddingData>,
    model: String,
    usage: OpenAiEmbeddingUsage,
}

#[derive(Deserialize)]
struct OpenAiEmbeddingData {
    object: String,
    embedding: Vec<f32>,
    index: usize,
}

#[derive(Deserialize)]
struct OpenAiEmbeddingUsage {
    prompt_tokens: u32,
    total_tokens: u32,
}

#[async_trait]
impl EmbeddingsProvider for OpenAiEmbeddingsProvider {
    async fn embed(&self, request: &EmbeddingRequest, _context: &RequestContext) -> Result<EmbeddingResponse> {
        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let model = Self::strip_model_prefix(&request.model).to_string();

        let wire_request = OpenAiEmbeddingRequest {
            input: request.input.as_vec().into_iter().map(String::from).collect(),
            model,
            encoding_format: request.encoding_format.clone(),
            dimensions: request.dimensions,
        };

        tracing::debug!(
            provider = %self.name,
            model = %request.model,
            "sending embeddings request"
        );

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key.expose_secret()))
            .json(&wire_request)
            .send()
            .await
            .map_err(|e| {
                tracing::error!(provider = %self.name, error = %e, "embeddings request failed");
                EmbeddingsError::ConnectionError(format!("Failed to send request to OpenAI embeddings: {e}"))
            })?;

        let status = response.status();

        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());

            tracing::error!(
                provider = %self.name,
                status = %status,
                "OpenAI embeddings API error"
            );

            return Err(match status.as_u16() {
                401 => EmbeddingsError::AuthenticationFailed(error_text),
                400 => EmbeddingsError::InvalidRequest(error_text),
                _ => EmbeddingsError::ProviderApiError {
                    status: status.as_u16(),
                    message: error_text,
                },
            });
        }

        let wire_response: OpenAiEmbeddingResponse = response.json().await.map_err(|e| {
            tracing::error!(
                provider = %self.name,
                error = %e,
                "failed to parse OpenAI embeddings response"
            );
            EmbeddingsError::InternalError(None)
        })?;

        tracing::debug!(provider = %self.name, "embeddings request complete");

        Ok(EmbeddingResponse {
            object: wire_response.object,
            data: wire_response
                .data
                .into_iter()
                .map(|d| crate::types::EmbeddingData {
                    object: d.object,
                    embedding: d.embedding,
                    index: d.index,
                })
                .collect(),
            model: wire_response.model,
            usage: crate::types::EmbeddingUsage {
                prompt_tokens: wire_response.usage.prompt_tokens,
                total_tokens: wire_response.usage.total_tokens,
            },
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}
