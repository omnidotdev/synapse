use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use http::{HeaderMap, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use synapse_auth::ApiKeyResolver;

/// Constant-time string comparison to prevent timing side-channel attacks
pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

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

    let expected = state.gateway_secret.expose_secret();
    let authorized = secret.is_some_and(|s| constant_time_eq(s, expected));
    if !authorized {
        return StatusCode::UNAUTHORIZED;
    }

    state.resolver.invalidate(&body.key);
    StatusCode::NO_CONTENT
}
