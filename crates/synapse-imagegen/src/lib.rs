#![allow(
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_const_for_fn,
    clippy::module_name_repetitions
)]

mod edit;
mod error;
#[cfg(feature = "billing")]
mod pricing;
mod provider;
mod server;
mod types;

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, State, multipart::MultipartRejection},
    routing::post,
};
use synapse_core::RequestContext;

pub use edit::{FormPart, ImageEditRequest};
pub use error::{ImageGenError, Result};
pub use types::{ImageData, ImageRequest, ImageResponse, ImageUsage, TokenDetails};

use server::ImageGenServerBuilder;
pub use server::Server;

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

/// Body limit for image edits (64 MiB): up to 16 reference images plus a
/// mask, well above the default 2 MiB request limit
const EDIT_BODY_LIMIT_BYTES: usize = 64 << 20;

/// Create the endpoint router for image generation
pub fn endpoint_router() -> Router<Arc<Server>> {
    Router::new().route("/v1/images/generations", post(generate)).route(
        "/v1/images/edits",
        post(edit).layer(DefaultBodyLimit::max(EDIT_BODY_LIMIT_BYTES)),
    )
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

/// Handle image edit requests (multipart form with reference images)
async fn edit(
    State(server): State<Arc<Server>>,
    axum::Extension(context): axum::Extension<RequestContext>,
    multipart: std::result::Result<Multipart, MultipartRejection>,
) -> Result<Json<ImageResponse>> {
    let parts = read_form(multipart.map_err(|e| ImageGenError::InvalidRequest(e.body_text()))?).await?;
    let request = ImageEditRequest::from_parts(parts)?;

    tracing::debug!(
        model = %request.model,
        images = request.image_count,
        "Image edit handler called"
    );

    let response = server.edit(&request, &context).await?;

    tracing::debug!("Image edit complete");

    Ok(Json(response))
}

/// Read every part of a multipart form
async fn read_form(mut multipart: Multipart) -> Result<Vec<FormPart>> {
    let mut parts = Vec::new();

    while let Some(field) = multipart.next_field().await.map_err(|e| form_error(&e))? {
        let name = field.name().unwrap_or_default().to_owned();
        let file_name = field.file_name().map(str::to_owned);
        let content_type = field.content_type().map(str::to_owned);
        let data = field.bytes().await.map_err(|e| form_error(&e))?;

        parts.push(FormPart {
            name,
            file_name,
            content_type,
            data,
        });
    }

    Ok(parts)
}

/// Map a multipart read failure, keeping an over-limit body a 413
fn form_error(error: &axum::extract::multipart::MultipartError) -> ImageGenError {
    if error.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
        ImageGenError::PayloadTooLarge(format!(
            "image edit forms are limited to {} MiB",
            EDIT_BODY_LIMIT_BYTES >> 20
        ))
    } else {
        ImageGenError::InvalidRequest(format!("failed to read multipart form: {}", error.body_text()))
    }
}
