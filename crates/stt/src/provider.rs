pub mod deepgram;
pub mod whisper;

use async_trait::async_trait;

use crate::{
    request::RequestContext,
    types::{TranscriptionRequest, TranscriptionResponse},
};

/// Trait for STT provider implementations
#[async_trait]
pub trait SttProvider: Send + Sync {
    /// Transcribe audio to text
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
        context: &RequestContext,
    ) -> crate::error::Result<TranscriptionResponse>;

    /// Get the provider name
    fn name(&self) -> &str;
}
