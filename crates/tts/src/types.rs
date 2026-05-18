use serde::Deserialize;

/// Speech synthesis request following `OpenAI` TTS API format
#[derive(Debug, Deserialize)]
pub struct SpeechRequest {
    /// Model identifier (e.g. "tts-1" or "`eleven_multilingual_v2`")
    pub model: String,
    /// Text to synthesize into speech
    pub input: String,
    /// Voice identifier (e.g. "alloy" or an `ElevenLabs` voice ID)
    pub voice: String,
    /// Output audio format (mp3, opus, aac, flac, wav, pcm)
    pub response_format: Option<String>,
    /// Speech speed multiplier (0.25 to 4.0)
    pub speed: Option<f64>,
}

/// Raw audio response from a TTS provider
pub struct SpeechResponse {
    /// Raw audio bytes
    pub audio: Vec<u8>,
    /// Content type of the audio (e.g. "audio/mpeg")
    pub content_type: String,
}

impl SpeechResponse {
    /// Convert the speech response into an axum HTTP response
    ///
    /// # Panics
    ///
    /// Panics if the fallback error response cannot be constructed
    pub fn into_response(self) -> axum::response::Response {
        axum::response::Response::builder()
            .header(http::header::CONTENT_TYPE, self.content_type)
            .body(axum::body::Body::from(self.audio))
            .unwrap_or_else(|_| {
                axum::response::Response::builder()
                    .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                    .body(axum::body::Body::empty())
                    .unwrap()
            })
    }
}
