pub mod elevenlabs;
pub mod openai_tts;

use async_trait::async_trait;
use secrecy::SecretString;

use crate::{
    request::RequestContext,
    types::{SpeechRequest, SpeechResponse},
};

/// Trait for TTS provider implementations
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Synthesize text to speech
    ///
    /// `api_key` overrides the provider's configured key (the caller's own
    /// key on a BYOK request); `None` uses the configured key
    async fn synthesize(
        &self,
        request: SpeechRequest,
        context: &RequestContext,
        api_key: Option<&SecretString>,
    ) -> crate::error::Result<SpeechResponse>;

    /// Get the provider name
    fn name(&self) -> &str;

    /// The upstream vendor whose API this provider calls (e.g. `openai` for
    /// Whisper), which is the name a caller's BYOK key is registered under
    fn key_provider(&self) -> &'static str;
}
