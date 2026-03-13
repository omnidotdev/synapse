use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, ImageGenError>;

/// Image generation service errors with appropriate HTTP status codes
#[derive(Debug, Error)]
pub enum ImageGenError {
    /// Invalid request parameters
    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    /// Authentication failed (missing or invalid API key)
    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    /// Provider not found in configuration
    #[error("Provider '{0}' not found")]
    ProviderNotFound(String),

    /// Provider API returned an error
    #[error("Provider API error ({status}): {message}")]
    ProviderApiError { status: u16, message: String },

    /// Network or connection error
    #[error("Connection error: {0}")]
    ConnectionError(String),

    /// Configuration error
    #[error("Configuration error: {0}")]
    ConfigError(String),

    /// Internal server error
    /// If Some(message), it came from a provider and can be shown
    /// If None, it's an internal error and should not leak details
    #[error("Internal server error")]
    InternalError(Option<String>),
}

impl ImageGenError {
    /// Get the appropriate HTTP status code for this error
    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Self::AuthenticationFailed(_) => StatusCode::UNAUTHORIZED,
            Self::ProviderNotFound(_) => StatusCode::NOT_FOUND,
            Self::ConnectionError(_) => StatusCode::BAD_GATEWAY,
            Self::ProviderApiError { status, .. } => match *status {
                400 => StatusCode::BAD_REQUEST,
                401 => StatusCode::UNAUTHORIZED,
                403 => StatusCode::FORBIDDEN,
                429 => StatusCode::TOO_MANY_REQUESTS,
                _ => StatusCode::BAD_GATEWAY,
            },
            Self::ConfigError(_) | Self::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Get the error type string for the response
    pub fn error_type(&self) -> &str {
        match self {
            Self::InvalidRequest(_) => "invalid_request_error",
            Self::AuthenticationFailed(_) => "authentication_error",
            Self::ProviderNotFound(_) => "not_found_error",
            Self::ConnectionError(_) | Self::ProviderApiError { .. } => "api_error",
            Self::ConfigError(_) | Self::InternalError(_) => "internal_error",
        }
    }

    /// Message that is safe to expose to API consumers
    pub fn client_message(&self) -> String {
        match self {
            Self::InternalError(Some(provider_msg)) => provider_msg.clone(),
            Self::InternalError(None) => "Internal server error".to_string(),
            _ => self.to_string(),
        }
    }
}

/// Error response format compatible with `OpenAI` API
#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: ErrorDetails,
}

#[derive(Debug, Serialize)]
struct ErrorDetails {
    message: String,
    r#type: String,
    code: u16,
}

impl IntoResponse for ImageGenError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let message = self.client_message();

        let error_response = ErrorResponse {
            error: ErrorDetails {
                message,
                r#type: self.error_type().to_string(),
                code: status.as_u16(),
            },
        };

        (status, Json(error_response)).into_response()
    }
}
