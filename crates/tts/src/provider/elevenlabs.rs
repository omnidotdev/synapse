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

const DEFAULT_ELEVENLABS_API_URL: &str = "https://api.elevenlabs.io/v1";

/// `ElevenLabs` TTS provider
pub struct ElevenLabsProvider {
    client: Client,
    base_url: String,
    api_key: SecretString,
    name: String,
}

impl ElevenLabsProvider {
    pub fn new(name: String, api_key: SecretString, base_url: Option<String>) -> Self {
        let client = http_client();
        let base_url = base_url.unwrap_or_else(|| DEFAULT_ELEVENLABS_API_URL.to_string());

        Self {
            client,
            base_url,
            api_key,
            name,
        }
    }
}

#[derive(serde::Serialize)]
struct ElevenLabsRequest<'a> {
    text: &'a str,
    model_id: &'a str,
}

#[async_trait]
impl TtsProvider for ElevenLabsProvider {
    async fn synthesize(
        &self,
        request: SpeechRequest,
        _context: &RequestContext,
    ) -> crate::error::Result<SpeechResponse> {
        let url = format!("{}/text-to-speech/{}", self.base_url, request.voice);

        tracing::debug!(
            "ElevenLabs TTS request: model={}, voice={}, input_len={}",
            request.model,
            request.voice,
            request.input.len(),
        );

        let body = ElevenLabsRequest {
            text: &request.input,
            model_id: &request.model,
        };

        let response = self
            .client
            .post(&url)
            .header("xi-api-key", self.api_key.expose_secret().to_string())
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("ElevenLabs request failed: {e}");
                TtsError::ConnectionError(format!("Failed to send request to ElevenLabs: {e}"))
            })?;

        let status = response.status();

        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());

            tracing::error!("ElevenLabs API error ({status}): {error_text}");

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
            tracing::error!("Failed to read ElevenLabs response body: {e}");
            TtsError::InternalError(None)
        })?;

        tracing::debug!("ElevenLabs TTS synthesis complete, {} bytes", audio.len());

        Ok(SpeechResponse {
            audio: audio.to_vec(),
            content_type,
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}
