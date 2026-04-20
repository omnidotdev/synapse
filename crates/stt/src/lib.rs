#![allow(
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_const_for_fn,
    clippy::module_name_repetitions
)]

mod error;
mod http_client;
mod provider;
mod request;
mod server;
mod types;

use std::sync::Arc;

use axum::{Json, Router, extract::State, routing::post};

pub use error::{Result, SttError};
use request::ExtractMultipart;
pub use request::RequestContext;
pub use server::{Server, SttServerBuilder};
pub use types::{TranscriptionRequest, TranscriptionResponse};

/// Build the STT server from configuration
///
/// # Errors
///
/// Returns an error if the server fails to initialize
pub fn build_server(config: &synapse_config::Config) -> anyhow::Result<Arc<Server>> {
    let server = Arc::new(
        SttServerBuilder::new(config)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to initialize STT server: {e}"))?,
    );
    Ok(server)
}

/// Create the endpoint router for STT
pub fn endpoint_router() -> Router<Arc<Server>> {
    Router::new().route("/v1/audio/transcriptions", post(transcribe))
}

/// Handle transcription requests
async fn transcribe(
    State(server): State<Arc<Server>>,
    ExtractMultipart(context, request): ExtractMultipart,
) -> Result<Json<types::TranscriptionResponse>> {
    tracing::debug!("STT transcription handler called for model: {}", request.model);

    let response = server.transcribe(request, &context).await?;

    tracing::debug!("Transcription complete");

    Ok(Json(response))
}
