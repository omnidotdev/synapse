pub mod openai;

use async_trait::async_trait;
use synapse_core::RequestContext;

use crate::{
    error::Result,
    types::{ImageRequest, ImageResponse},
};

/// Trait for image generation provider implementations
#[async_trait]
pub trait ImageGenProvider: Send + Sync {
    /// Generate images for the given request
    async fn generate(&self, request: &ImageRequest, context: &RequestContext) -> Result<ImageResponse>;

    /// Get the provider name
    fn name(&self) -> &str;
}
