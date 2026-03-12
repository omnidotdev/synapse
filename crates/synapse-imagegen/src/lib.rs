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

pub use error::{ImageGenError, Result};
pub use types::{ImageData, ImageRequest, ImageResponse};

use server::{ImageGenServerBuilder, Server};

/// Build the image generation server from configuration
///
/// # Errors
///
/// Returns an error if the server fails to initialize
pub fn build_server(config: &synapse_config::Config) -> anyhow::Result<Arc<Server>> {
    let server = Arc::new(
        ImageGenServerBuilder::new(config)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to initialize image generation server: {e}"))?,
    );
    Ok(server)
}

/// Create the endpoint router for image generation
pub fn endpoint_router() -> Router<Arc<Server>> {
    Router::new().route("/v1/images/generations", post(generate))
}

/// Handle image generation requests
async fn generate(
    State(server): State<Arc<Server>>,
    axum::Extension(context): axum::Extension<RequestContext>,
    Json(request): Json<ImageRequest>,
) -> Result<Json<ImageResponse>> {
    tracing::debug!("Image generation handler called for model: {}", request.model);

    let response = server.generate(&request, &context).await?;

    tracing::debug!("Image generation complete");

    Ok(Json(response))
}
