pub mod openai;

use async_trait::async_trait;
use synapse_core::RequestContext;

use crate::{
    error::Result,
    types::{EmbeddingRequest, EmbeddingResponse},
};

/// Trait for embeddings provider implementations
#[async_trait]
pub trait EmbeddingsProvider: Send + Sync {
    /// Generate embeddings for the given request
    async fn embed(&self, request: &EmbeddingRequest, context: &RequestContext) -> Result<EmbeddingResponse>;

    /// Get the provider name
    fn name(&self) -> &str;
}
