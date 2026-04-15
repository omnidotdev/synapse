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

const DEFAULT_OPENAI_API_URL: &str = "https://api.openai.com/v1";

/// `OpenAI` Whisper STT provider
pub struct WhisperProvider {
    client: Client,
    base_url: String,
    api_key: SecretString,
    name: String,
}

impl WhisperProvider {
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

#[derive(serde::Deserialize)]
struct WhisperResponse {
    text: String,
}

#[async_trait]
impl SttProvider for WhisperProvider {
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
        _context: &RequestContext,
    ) -> crate::error::Result<TranscriptionResponse> {
        let url = format!("{}/audio/transcriptions", self.base_url);

        tracing::debug!(
            "Whisper transcription request: {} bytes, model={}",
            request.audio.len(),
            request.model,
        );

        let mut form = reqwest::multipart::Form::new()
            .part(
                "file",
                reqwest::multipart::Part::bytes(request.audio)
                    .file_name(request.filename)
                    .mime_str(&request.content_type)
                    .map_err(|e| SttError::InvalidRequest(format!("Invalid content type: {e}")))?,
            )
            .text("model", request.model);

        if let Some(language) = request.language {
            form = form.text("language", language);
        }

        if let Some(prompt) = request.prompt {
            form = form.text("prompt", prompt);
        }

        if let Some(response_format) = request.response_format {
            form = form.text("response_format", response_format);
        }

        if let Some(temperature) = request.temperature {
            form = form.text("temperature", temperature.to_string());
        }

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key.expose_secret()))
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                tracing::error!("Whisper request failed: {e}");
                SttError::ConnectionError(format!("Failed to send request to Whisper: {e}"))
            })?;

        let status = response.status();

        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());

            tracing::error!("Whisper API error ({status}): {error_text}");

            return Err(match status.as_u16() {
                401 => SttError::AuthenticationFailed(error_text),
                400 => SttError::InvalidRequest(error_text),
                _ => SttError::ProviderApiError {
                    status: status.as_u16(),
                    message: error_text,
                },
            });
        }

        let result: WhisperResponse = response.json().await.map_err(|e| {
            tracing::error!("Failed to parse Whisper response: {e}");
            SttError::InternalError(None)
        })?;

        tracing::debug!("Whisper transcription complete");

        Ok(TranscriptionResponse { text: result.text })
    }

    fn name(&self) -> &str {
        &self.name
    }
}
