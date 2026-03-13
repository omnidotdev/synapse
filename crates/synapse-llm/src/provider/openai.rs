//! OpenAI-compatible provider implementation

use std::pin::Pin;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use synapse_config::LlmProviderConfig;
use synapse_core::{HeaderRule, RequestContext, apply_header_rules};
use url::Url;

use super::{Provider, ProviderCapabilities};
use crate::convert::openai::openai_chunk_to_events;
use crate::error::LlmError;
use crate::protocol::openai::{OpenAiRequest, OpenAiResponse, OpenAiStreamChunk};
use crate::types::{CompletionRequest, CompletionResponse, StreamEvent};

/// Default `OpenAI` API base URL
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Whether the provider is the canonical `OpenAI` API (vs a compatible third-party)
fn is_canonical_openai(base_url: &Url) -> bool {
    base_url.host_str().is_some_and(|h| h == "api.openai.com")
}

/// OpenAI-compatible provider
pub struct OpenAiProvider {
    name: String,
    client: Client,
    base_url: Url,
    api_key: Option<SecretString>,
    header_rules: Vec<HeaderRule>,
    forward_authorization: bool,
}

impl OpenAiProvider {
    /// Create from provider configuration
    ///
    /// # Errors
    ///
    /// Returns `LlmError::Internal` if the base URL is invalid.
    ///
    /// # Panics
    ///
    /// Panics if the hardcoded default base URL is invalid (should never happen).
    pub fn new(name: String, config: &LlmProviderConfig) -> Result<Self, LlmError> {
        let base_url = config
            .base_url
            .clone()
            .unwrap_or_else(|| Url::parse(DEFAULT_BASE_URL).expect("valid default URL"));

        let header_rules = super::parse_header_rules(&config.headers);

        Ok(Self {
            name,
            client: Client::new(),
            base_url,
            api_key: config.api_key.clone(),
            header_rules,
            forward_authorization: config.forward_authorization,
        })
    }

    /// Resolve the API key from config or request context
    fn resolve_api_key(&self, context: &RequestContext) -> Option<String> {
        // Prefer forwarded key from context
        if self.forward_authorization
            && let Some(key) = &context.api_key
        {
            return Some(key.expose_secret().to_owned());
        }
        // Fall back to configured key
        self.api_key.as_ref().map(|k| k.expose_secret().to_owned())
    }

    /// Build the chat completions URL
    fn completions_url(&self) -> String {
        let base = self.base_url.as_str().trim_end_matches('/');
        format!("{base}/chat/completions")
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
        }
    }

    async fn complete(
        &self,
        request: &CompletionRequest,
        context: &RequestContext,
    ) -> Result<CompletionResponse, LlmError> {
        let wire_request: OpenAiRequest = request.into();

        let api_key = self.resolve_api_key(context);
        let extra_headers = apply_header_rules(context.headers(), &self.header_rules);

        let mut builder = self
            .client
            .post(self.completions_url())
            .json(&wire_request)
            .headers(extra_headers);

        if let Some(key) = &api_key {
            builder = builder.bearer_auth(key);
        }

        let response = builder.send().await.map_err(|e| {
            tracing::error!(provider = %self.name, error = %e, "upstream request failed");
            LlmError::Upstream(e.to_string())
        })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::warn!(
                provider = %self.name,
                status = %status,
                "upstream returned error"
            );

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(LlmError::RateLimited { retry_after: 0 });
            }

            if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
                return Err(LlmError::Unauthorized);
            }

            return Err(LlmError::Upstream(format!("provider returned {status}: {body}")));
        }

        let wire_response: OpenAiResponse = response
            .json()
            .await
            .map_err(|e| LlmError::Upstream(format!("failed to parse response: {e}")))?;

        Ok(wire_response.into())
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
        context: &RequestContext,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>, LlmError> {
        let mut wire_request: OpenAiRequest = request.into();
        wire_request.stream = Some(true);

        // Only send stream_options to canonical OpenAI — many compatible
        // APIs (NVIDIA NIM, etc.) reject the unsupported parameter
        wire_request.stream_options = if is_canonical_openai(&self.base_url) {
            Some(crate::protocol::openai::OpenAiStreamOptions { include_usage: true })
        } else {
            None
        };

        let api_key = self.resolve_api_key(context);
        let extra_headers = apply_header_rules(context.headers(), &self.header_rules);

        let mut builder = self
            .client
            .post(self.completions_url())
            .json(&wire_request)
            .headers(extra_headers);

        if let Some(key) = &api_key {
            builder = builder.bearer_auth(key);
        }

        let response = builder.send().await.map_err(|e| {
            tracing::error!(provider = %self.name, error = %e, "upstream stream request failed");
            LlmError::Upstream(e.to_string())
        })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::warn!(
                provider = %self.name,
                status = %status,
                "upstream returned error"
            );

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(LlmError::RateLimited { retry_after: 0 });
            }

            if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
                return Err(LlmError::Unauthorized);
            }

            return Err(LlmError::Upstream(format!("provider returned {status}: {body}")));
        }

        let byte_stream = response.bytes_stream();
        let event_stream = byte_stream.eventsource();

        let mapped = event_stream
            .map(|result| match result {
                Ok(event) => {
                    let data = event.data.trim().to_owned();
                    if data == "[DONE]" {
                        return vec![Ok(StreamEvent::Done)];
                    }

                    match serde_json::from_str::<OpenAiStreamChunk>(&data) {
                        Ok(chunk) => openai_chunk_to_events(&chunk).into_iter().map(Ok).collect(),
                        Err(e) => {
                            tracing::debug!(error = %e, data = %data, "skipping unparseable SSE chunk");
                            vec![]
                        }
                    }
                }
                Err(e) => vec![Err(LlmError::Streaming(e.to_string()))],
            })
            .flat_map(futures_util::stream::iter);

        Ok(Box::pin(mapped))
    }
}
