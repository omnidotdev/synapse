#![allow(
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_const_for_fn,
    clippy::module_name_repetitions
)]

mod error;
mod provider;
mod server;
mod types;

use std::sync::Arc;

use axum::{Json, Router, extract::State, routing::post};
use synapse_core::RequestContext;

pub use error::{EmbeddingsError, Result};
pub use types::{EmbedInput, EmbeddingData, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage};

use server::{EmbeddingsServerBuilder, Server};

/// Build the embeddings server from configuration
///
/// # Errors
///
/// Returns an error if the server fails to initialize
pub fn build_server(config: &synapse_config::Config) -> anyhow::Result<Arc<Server>> {
    let server = Arc::new(
        EmbeddingsServerBuilder::new(config)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to initialize embeddings server: {e}"))?,
    );
    Ok(server)
}

/// Create the endpoint router for embeddings
pub fn endpoint_router() -> Router<Arc<Server>> {
    Router::new().route("/v1/embeddings", post(embed))
}

/// Handle embedding requests
async fn embed(
    State(server): State<Arc<Server>>,
    axum::Extension(context): axum::Extension<RequestContext>,
    Json(request): Json<EmbeddingRequest>,
) -> Result<Json<EmbeddingResponse>> {
    tracing::debug!("Embeddings handler called for model: {}", request.model);

    let response = server.embed(&request, &context).await?;

    tracing::debug!("Embedding generation complete");

    Ok(Json(response))
}
