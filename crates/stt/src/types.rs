use serde::{Deserialize, Serialize};

/// Transcription request following `OpenAI` Whisper API format
#[derive(Debug)]
pub struct TranscriptionRequest {
    /// Raw audio data
    pub audio: Vec<u8>,
    /// Original filename
    pub filename: String,
    /// Content type of the audio file
    pub content_type: String,
    /// Model identifier (e.g. "whisper-1" or "nova-2")
    pub model: String,
    /// Optional language hint (ISO 639-1)
    pub language: Option<String>,
    /// Optional prompt to guide transcription
    pub prompt: Option<String>,
    /// Response format (json, text, srt, `verbose_json`, vtt)
    pub response_format: Option<String>,
    /// Sampling temperature (0-1)
    pub temperature: Option<f32>,
}

/// Transcription response following `OpenAI` Whisper API format
#[derive(Debug, Serialize, Deserialize)]
pub struct TranscriptionResponse {
    /// Transcribed text
    pub text: String,
}
