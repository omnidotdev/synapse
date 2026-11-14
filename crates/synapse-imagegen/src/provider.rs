pub mod openai;

use async_trait::async_trait;
use synapse_core::RequestContext;

use crate::{
    edit::ImageEditRequest,
    error::{ImageGenError, Result},
    types::{ImageRequest, ImageResponse},
};

/// Trait for image generation provider implementations
///
/// `context.api_key`, when set, is the key to call the provider with (a BYOK
/// key); otherwise the provider's configured key is used
#[async_trait]
pub trait ImageGenProvider: Send + Sync {
    /// Generate images for the given request
    async fn generate(&self, request: &ImageRequest, context: &RequestContext) -> Result<ImageResponse>;

    /// Edit or compose images from reference images
    async fn edit(&self, _request: &ImageEditRequest, _context: &RequestContext) -> Result<ImageResponse> {
        Err(ImageGenError::InvalidRequest(format!(
            "provider '{}' does not support image edits",
            self.name()
        )))
    }

    /// Get the provider name
    fn name(&self) -> &str;

    /// The upstream vendor whose API this provider calls (e.g. `openai` for
    /// Whisper), which is the name a caller's BYOK key is registered under
    fn key_provider(&self) -> &'static str;
}
