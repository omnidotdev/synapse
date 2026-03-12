//! Anthropic Messages API provider implementation

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
use crate::convert::anthropic::AnthropicStreamState;
use crate::error::LlmError;
use crate::protocol::anthropic::{AnthropicRequest, AnthropicResponse, AnthropicStreamEvent};
use crate::types::{CompletionRequest, CompletionResponse, StreamEvent};

/// Default Anthropic API base URL
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

/// Anthropic API version header value
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic Messages API provider
pub struct AnthropicProvider {
    name: String,
    client: Client,
    base_url: Url,
    api_key: Option<SecretString>,
    header_rules: Vec<HeaderRule>,
    forward_authorization: bool,
}

impl AnthropicProvider {
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
        if self.forward_authorization
            && let Some(key) = &context.api_key
        {
            return Some(key.expose_secret().to_owned());
        }
        self.api_key.as_ref().map(|k| k.expose_secret().to_owned())
    }

    /// Build the messages endpoint URL
    fn messages_url(&self) -> String {
        let base = self.base_url.as_str().trim_end_matches('/');
        format!("{base}/messages")
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
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
        let wire_request: AnthropicRequest = request.into();

        let api_key = self.resolve_api_key(context);
        let extra_headers = apply_header_rules(context.headers(), &self.header_rules);

        let mut builder = self
            .client
            .post(self.messages_url())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&wire_request)
            .headers(extra_headers);

        if let Some(key) = &api_key {
            builder = builder.header("x-api-key", key);
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

        let wire_response: AnthropicResponse = response
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
        let mut wire_request: AnthropicRequest = request.into();
        wire_request.stream = Some(true);

        let api_key = self.resolve_api_key(context);
        let extra_headers = apply_header_rules(context.headers(), &self.header_rules);

        let mut builder = self
            .client
            .post(self.messages_url())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&wire_request)
            .headers(extra_headers);

        if let Some(key) = &api_key {
            builder = builder.header("x-api-key", key);
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

        let event_stream = response.bytes_stream().eventsource();
        let mut state = AnthropicStreamState::new();

        let mapped = event_stream.filter_map(move |result| {
            let events: Option<Result<StreamEvent, LlmError>> = match &result {
                Ok(event) => {
                    let data = event.data.trim();
                    if data.is_empty() {
                        None
                    } else {
                        match serde_json::from_str::<AnthropicStreamEvent>(data) {
                            Ok(stream_event) => {
                                let converted = state.convert_event(&stream_event);
                                converted.into_iter().next().map(Ok)
                            }
                            Err(e) => {
                                tracing::debug!(
                                    error = %e,
                                    "skipping unparseable Anthropic SSE event"
                                );
                                None
                            }
                        }
                    }
                }
                Err(e) => Some(Err(LlmError::Streaming(e.to_string()))),
            };

            async move { events }
        });

        Ok(Box::pin(mapped))
    }
}
