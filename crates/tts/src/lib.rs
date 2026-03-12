#![allow(clippy::must_use_candidate, clippy::missing_errors_doc)]

mod error;
mod http_client;
mod provider;
mod request;
mod server;
mod types;

use std::sync::Arc;

use axum::{Router, extract::State, routing::post};

pub use error::{Result, TtsError};
use request::ExtractPayload;
pub use request::RequestContext;
pub use server::{Server, TtsServerBuilder};
pub use types::{SpeechRequest, SpeechResponse};

/// Build the TTS server from configuration
pub fn build_server(config: &synapse_config::Config) -> anyhow::Result<Arc<Server>> {
    let server = Arc::new(
        TtsServerBuilder::new(config)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to initialize TTS server: {e}"))?,
    );
    Ok(server)
}

/// Create the endpoint router for TTS
pub fn endpoint_router() -> Router<Arc<Server>> {
    Router::new().route("/v1/audio/speech", post(synthesize))
}

/// Handle speech synthesis requests
async fn synthesize(
    State(server): State<Arc<Server>>,
    ExtractPayload(context, request): ExtractPayload<types::SpeechRequest>,
) -> Result<axum::response::Response> {
    tracing::debug!("TTS speech handler called for model: {}", request.model);

    let response = server.synthesize(request, &context).await?;

    tracing::debug!("Speech synthesis complete");

    Ok(response.into_response())
}
