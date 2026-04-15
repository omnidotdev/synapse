use axum::response::IntoResponse;
use http::StatusCode;

/// Health check handler
pub async fn health_handler() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}
