use async_trait::async_trait;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};

use crate::{
    error::TtsError,
    http_client::http_client,
    request::RequestContext,
    types::{SpeechRequest, SpeechResponse},
};

use super::TtsProvider;

const DEFAULT_OPENAI_API_URL: &str = "https://api.openai.com/v1";

/// `OpenAI` TTS provider
pub struct OpenAiTtsProvider {
    client: Client,
    base_url: String,
    api_key: SecretString,
    name: String,
}

impl OpenAiTtsProvider {
    pub fn new(name: String, api_key: SecretString, base_url: Option<String>) -> Self {
        let client = http_client();
        let base_url = base_url.unwrap_or_else(|| DEFAULT_OPENAI_API_URL.to_string());

        Self {
            client,
            base_url,
            api_key,
            name,
        }
    }
}

#[derive(serde::Serialize)]
struct OpenAiTtsRequest<'a> {
    model: &'a str,
    input: &'a str,
    voice: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed: Option<f64>,
}

#[async_trait]
impl TtsProvider for OpenAiTtsProvider {
    async fn synthesize(
        &self,
        request: SpeechRequest,
        _context: &RequestContext,
    ) -> crate::error::Result<SpeechResponse> {
        let url = format!("{}/audio/speech", self.base_url);

        tracing::debug!(
            "OpenAI TTS request: model={}, voice={}, input_len={}",
            request.model,
            request.voice,
            request.input.len(),
        );

        let body = OpenAiTtsRequest {
            model: &request.model,
            input: &request.input,
            voice: &request.voice,
            response_format: request.response_format.as_deref(),
            speed: request.speed,
        };

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key.expose_secret()))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("OpenAI TTS request failed: {e}");
                TtsError::ConnectionError(format!("Failed to send request to OpenAI TTS: {e}"))
            })?;

        let status = response.status();

        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());

            tracing::error!("OpenAI TTS API error ({status}): {error_text}");

            return Err(match status.as_u16() {
                401 => TtsError::AuthenticationFailed(error_text),
                400 => TtsError::InvalidRequest(error_text),
                _ => TtsError::ProviderApiError {
                    status: status.as_u16(),
                    message: error_text,
                },
            });
        }

        let content_type = response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("audio/mpeg")
            .to_string();

        let audio = response.bytes().await.map_err(|e| {
            tracing::error!("Failed to read OpenAI TTS response body: {e}");
            TtsError::InternalError(None)
        })?;

        tracing::debug!("OpenAI TTS synthesis complete, {} bytes", audio.len());

        Ok(SpeechResponse {
            audio: audio.to_vec(),
            content_type,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}
