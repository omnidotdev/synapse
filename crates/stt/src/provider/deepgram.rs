use async_trait::async_trait;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};

use crate::{
    error::SttError,
    http_client::http_client,
    request::RequestContext,
    types::{TranscriptionRequest, TranscriptionResponse},
};

use super::SttProvider;

const DEFAULT_DEEPGRAM_API_URL: &str = "https://api.deepgram.com/v1";

/// Deepgram STT provider
pub struct DeepgramProvider {
    client: Client,
    base_url: String,
    api_key: SecretString,
    name: String,
}

impl DeepgramProvider {
    pub fn new(name: String, api_key: SecretString, base_url: Option<String>) -> Self {
        let client = http_client();
        let base_url = base_url.unwrap_or_else(|| DEFAULT_DEEPGRAM_API_URL.to_string());

        Self {
            client,
            base_url,
            api_key,
            name,
        }
    }
}

#[derive(serde::Deserialize)]
struct DeepgramResponse {
    results: DeepgramResults,
}

#[derive(serde::Deserialize)]
struct DeepgramResults {
    channels: Vec<DeepgramChannel>,
}

#[derive(serde::Deserialize)]
struct DeepgramChannel {
    alternatives: Vec<DeepgramAlternative>,
}

#[derive(serde::Deserialize)]
struct DeepgramAlternative {
    transcript: String,
}

#[async_trait]
impl SttProvider for DeepgramProvider {
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
        _context: &RequestContext,
    ) -> crate::error::Result<TranscriptionResponse> {
        let mut url = format!("{}/listen?model={}&punctuate=true", self.base_url, request.model);

        if let Some(language) = &request.language {
            use std::fmt::Write;
            let _ = write!(url, "&language={language}");
        }

        tracing::debug!(
            "Deepgram transcription request: {} bytes, model={}",
            request.audio.len(),
            request.model,
        );

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Token {}", self.api_key.expose_secret()))
            .header("Content-Type", &request.content_type)
            .body(request.audio)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("Deepgram request failed: {e}");
                SttError::ConnectionError(format!("Failed to send request to Deepgram: {e}"))
            })?;

        let status = response.status();

        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());

            tracing::error!("Deepgram API error ({status}): {error_text}");

            return Err(match status.as_u16() {
                401 => SttError::AuthenticationFailed(error_text),
                400 => SttError::InvalidRequest(error_text),
                _ => SttError::ProviderApiError {
                    status: status.as_u16(),
                    message: error_text,
                },
            });
        }

        let result: DeepgramResponse = response.json().await.map_err(|e| {
            tracing::error!("Failed to parse Deepgram response: {e}");
            SttError::InternalError(None)
        })?;

        let transcript = result
            .results
            .channels
            .first()
            .and_then(|c| c.alternatives.first())
            .map(|a| a.transcript.clone())
            .unwrap_or_default();

        tracing::debug!("Deepgram transcription complete");

        Ok(TranscriptionResponse { text: transcript })
    }

    fn name(&self) -> &str {
        &self.name
    }
}
