use axum::body::Body;
use secrecy::SecretString;
use serde::de::DeserializeOwned;
use synapse_core::{Authentication, ClientIdentity};

/// Header name for user-provided API keys (BYOK)
const PROVIDER_API_KEY_HEADER: &str = "X-Provider-API-Key";

/// Runtime context for TTS provider requests
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RequestContext {
    pub parts: http::request::Parts,

    /// User-provided API key that overrides the configured key
    pub api_key: Option<SecretString>,

    /// Client identity information for rate limiting and access control
    pub client_identity: Option<ClientIdentity>,

    pub authentication: Authentication,
}

#[allow(dead_code)]
impl RequestContext {
    pub const fn headers(&self) -> &axum::http::HeaderMap {
        &self.parts.headers
    }
}

/// Extractor for JSON request bodies
pub struct ExtractPayload<T>(pub RequestContext, pub T);

/// Body limit for TTS requests (1 MiB)
const BODY_LIMIT_BYTES: usize = 1 << 20;

static APPLICATION_JSON: http::HeaderValue = http::HeaderValue::from_static("application/json");

impl<S, T: DeserializeOwned> axum::extract::FromRequest<S> for ExtractPayload<T>
where
    S: Send + Sync,
{
    type Rejection = axum::response::Response;

    async fn from_request(request: http::Request<Body>, _state: &S) -> Result<Self, Self::Rejection> {
        use axum::response::IntoResponse;

        let (mut parts, body) = request.into_parts();

        if parts
            .headers
            .get(http::header::CONTENT_TYPE)
            .is_none_or(|value| value != APPLICATION_JSON)
        {
            return Err((
                axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Unsupported Content-Type, expected: 'Content-Type: application/json'",
            )
                .into_response());
        }

        let bytes = axum::body::to_bytes(body, BODY_LIMIT_BYTES).await.map_err(|err| {
            if std::error::Error::source(&err)
                .is_some_and(<dyn std::error::Error + 'static>::is::<http_body_util::LengthLimitError>)
            {
                (
                    axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                    format!("Request body is too large, limit is {BODY_LIMIT_BYTES} bytes"),
                )
            } else {
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    format!("Failed to read request body: {err}"),
                )
            }
            .into_response()
        })?;

        let body = match serde_json::from_slice::<T>(&bytes) {
            Ok(body) => body,
            Err(e) => {
                return Err((
                    axum::http::StatusCode::BAD_REQUEST,
                    format!("Failed to parse request body: {e}"),
                )
                    .into_response());
            }
        };

        let ctx = RequestContext {
            api_key: parts
                .headers
                .get(PROVIDER_API_KEY_HEADER)
                .and_then(|value| value.to_str().map(str::to_string).ok())
                .map(SecretString::from),
            client_identity: parts.extensions.remove(),
            authentication: parts.extensions.remove().unwrap_or_default(),
            parts,
        };

        Ok(Self(ctx, body))
    }
}
