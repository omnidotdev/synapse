pub mod elevenlabs;
pub mod openai_tts;

use async_trait::async_trait;

use crate::{
    request::RequestContext,
    types::{SpeechRequest, SpeechResponse},
};

/// Trait for TTS provider implementations
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Synthesize text to speech
    async fn synthesize(
        &self,
        request: SpeechRequest,
        context: &RequestContext,
    ) -> crate::error::Result<SpeechResponse>;

    /// Get the provider name
    fn name(&self) -> &str;
}
