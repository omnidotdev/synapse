use http::StatusCode;
use synapse_core::HttpError;
use thiserror::Error;

/// Errors that can occur during LLM operations
#[derive(Debug, Error)]
pub enum LlmError {
    /// Requested model was not found in any configured provider
    #[error("model not found: {model}")]
    ModelNotFound { model: String },

    /// Named provider does not exist in configuration
    #[error("provider not found: {provider}")]
    ProviderNotFound { provider: String },

    /// Upstream provider returned an error
    #[error("upstream error: {0}")]
    Upstream(String),

    /// Error during streaming response
    #[error("streaming error: {0}")]
    Streaming(String),

    /// Client sent a malformed or invalid request
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// Request lacks required authentication credentials
    #[error("authentication required")]
    Unauthorized,

    /// Client has exceeded their rate limit
    #[error("rate limit exceeded")]
    RateLimited {
        /// Seconds until the rate limit resets
        retry_after: u64,
    },

    /// Client has insufficient credits for the estimated request cost
    #[error("insufficient credits: {message}")]
    InsufficientCredits { message: String },

    /// Unexpected internal error
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl LlmError {
    /// Whether this error should trigger a failover attempt
    ///
    /// Retryable errors indicate a transient provider issue where trying
    /// an equivalent model on another provider may succeed.
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Upstream(_) | Self::Streaming(_) | Self::RateLimited { .. } | Self::Internal(_)
        )
    }
}

impl HttpError for LlmError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::ModelNotFound { .. } | Self::ProviderNotFound { .. } => StatusCode::NOT_FOUND,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::Streaming(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::InsufficientCredits { .. } => StatusCode::PAYMENT_REQUIRED,
        }
    }

    fn error_type(&self) -> &str {
        match self {
            Self::ModelNotFound { .. } | Self::ProviderNotFound { .. } => "not_found_error",
            Self::Upstream(_) => "upstream_error",
            Self::Streaming(_) => "streaming_error",
            Self::InvalidRequest(_) => "invalid_request_error",
            Self::Unauthorized => "authentication_error",
            Self::RateLimited { .. } => "rate_limit_error",
            Self::InsufficientCredits { .. } => "insufficient_credits_error",
            Self::Internal(_) => "internal_error",
        }
    }

    fn client_message(&self) -> String {
        match self {
            Self::Internal(_) => "an internal error occurred".to_owned(),
            other => other.to_string(),
        }
    }
}
