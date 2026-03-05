use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use http::{HeaderMap, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use synapse_auth::ApiKeyResolver;

/// Shared state for the cache invalidation endpoint
#[derive(Clone)]
pub struct InvalidateState {
    pub resolver: ApiKeyResolver,
    pub gateway_secret: SecretString,
}

/// Request body for cache invalidation
#[derive(Deserialize)]
pub struct InvalidateBody {
    pub key: String,
}

/// Invalidate a cached API key resolution
pub async fn invalidate_key_handler(
    State(state): State<InvalidateState>,
    headers: HeaderMap,
    Json(body): Json<InvalidateBody>,
) -> impl IntoResponse {
    let secret = headers.get("x-gateway-secret").and_then(|v| v.to_str().ok());

    if secret != Some(state.gateway_secret.expose_secret()) {
        return StatusCode::UNAUTHORIZED;
    }

    state.resolver.invalidate(&body.key);
    StatusCode::NO_CONTENT
}
