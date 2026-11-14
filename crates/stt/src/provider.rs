pub mod deepgram;
pub mod whisper;

use async_trait::async_trait;
use secrecy::SecretString;

use crate::{
    request::RequestContext,
    types::{TranscriptionRequest, TranscriptionResponse},
};

/// Trait for STT provider implementations
#[async_trait]
pub trait SttProvider: Send + Sync {
    /// Transcribe audio to text
    ///
    /// `api_key` overrides the provider's configured key (the caller's own
    /// key on a BYOK request); `None` uses the configured key
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
        context: &RequestContext,
        api_key: Option<&SecretString>,
    ) -> crate::error::Result<TranscriptionResponse>;

    /// Get the provider name
    fn name(&self) -> &str;

    /// The upstream vendor whose API this provider calls (e.g. `openai` for
    /// Whisper), which is the name a caller's BYOK key is registered under
    fn key_provider(&self) -> &'static str;
}
