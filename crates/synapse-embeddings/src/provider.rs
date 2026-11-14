pub mod openai;

use async_trait::async_trait;
use secrecy::SecretString;
use synapse_core::RequestContext;

use crate::{
    error::Result,
    types::{EmbeddingRequest, EmbeddingResponse},
};

/// Trait for embeddings provider implementations
#[async_trait]
pub trait EmbeddingsProvider: Send + Sync {
    /// Generate embeddings for the given request
    ///
    /// `api_key` overrides the provider's configured key (the caller's own
    /// key on a BYOK request); `None` uses the configured key
    async fn embed(
        &self,
        request: &EmbeddingRequest,
        context: &RequestContext,
        api_key: Option<&SecretString>,
    ) -> Result<EmbeddingResponse>;

    /// Get the provider name
    fn name(&self) -> &str;
}
