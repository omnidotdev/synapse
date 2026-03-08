//! Google Generative Language API provider implementation

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
use crate::convert::google::google_chunk_to_events;
use crate::error::LlmError;
use crate::protocol::google::{GoogleRequest, GoogleResponse};
use crate::types::{CompletionRequest, CompletionResponse, StreamEvent};

/// Default Google Generative Language API base URL
const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Google Generative Language API provider
pub struct GoogleProvider {
    name: String,
    client: Client,
    base_url: Url,
    api_key: Option<SecretString>,
    header_rules: Vec<HeaderRule>,
    forward_authorization: bool,
}

impl GoogleProvider {
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

    /// Build the `generateContent` endpoint URL for a model
    fn generate_url(&self, model: &str, api_key: Option<&str>) -> String {
        let base = self.base_url.as_str().trim_end_matches('/');
        let mut url = format!("{base}/models/{model}:generateContent");
        if let Some(key) = api_key {
            use std::fmt::Write;
            let _ = write!(url, "?key={key}");
        }
        url
    }

    /// Build the `streamGenerateContent` endpoint URL for a model
    fn stream_url(&self, model: &str, api_key: Option<&str>) -> String {
        let base = self.base_url.as_str().trim_end_matches('/');
        let mut url = format!("{base}/models/{model}:streamGenerateContent?alt=sse");
        if let Some(key) = api_key {
            use std::fmt::Write;
            let _ = write!(url, "&key={key}");
        }
        url
    }
}

#[async_trait]
impl Provider for GoogleProvider {
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
        let wire_request: GoogleRequest = request.into();
        let api_key = self.resolve_api_key(context);
        let extra_headers = apply_header_rules(context.headers(), &self.header_rules);

        let url = self.generate_url(&request.model, api_key.as_deref());

        let response = self
            .client
            .post(&url)
            .json(&wire_request)
            .headers(extra_headers)
            .send()
            .await
            .map_err(|e| {
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

            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                return Err(LlmError::Unauthorized);
            }

            return Err(LlmError::Upstream(format!("provider returned {status}: {body}")));
        }

        let wire_response: GoogleResponse = response
            .json()
            .await
            .map_err(|e| LlmError::Upstream(format!("failed to parse response: {e}")))?;

        let mut internal: CompletionResponse = wire_response.into();
        // Fill in the model name that Google doesn't include in the response
        internal.model.clone_from(&request.model);

        Ok(internal)
    }

    async fn complete_stream(
        &self,
        request: &CompletionRequest,
        context: &RequestContext,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>, LlmError> {
        let wire_request: GoogleRequest = request.into();
        let api_key = self.resolve_api_key(context);
        let extra_headers = apply_header_rules(context.headers(), &self.header_rules);

        let url = self.stream_url(&request.model, api_key.as_deref());

        let response = self
            .client
            .post(&url)
            .json(&wire_request)
            .headers(extra_headers)
            .send()
            .await
            .map_err(|e| {
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

            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                return Err(LlmError::Unauthorized);
            }

            return Err(LlmError::Upstream(format!("provider returned {status}: {body}")));
        }

        // Google streaming uses SSE with JSON data lines
        let event_stream = response.bytes_stream().eventsource();

        let mapped = event_stream.filter_map(|result| async move {
            match result {
                Ok(event) => {
                    let data = event.data.trim().to_owned();
                    if data.is_empty() {
                        return None;
                    }

                    match serde_json::from_str::<GoogleResponse>(&data) {
                        Ok(chunk) => {
                            let events = google_chunk_to_events(&chunk);
                            events.into_iter().next().map(Ok)
                        }
                        Err(e) => {
                            tracing::debug!(
                                error = %e,
                                data = %data,
                                "skipping unparseable Google SSE chunk"
                            );
                            None
                        }
                    }
                }
                Err(e) => Some(Err(LlmError::Streaming(e.to_string()))),
            }
        });

        Ok(Box::pin(mapped))
    }
}
