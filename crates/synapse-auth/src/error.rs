/// Authentication errors
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// Invalid or unrecognized API key
    #[error("invalid API key")]
    InvalidKey,

    /// API key has expired
    #[error("expired API key")]
    ExpiredKey,

    /// HTTP request to synapse-api failed
    #[error("key resolution failed: {0}")]
    ResolutionFailed(#[from] reqwest::Error),

    /// synapse-api returned a non-success response
    #[error("synapse-api error ({status}): {message}")]
    ApiError {
        /// HTTP status code
        status: u16,
        /// Error message from API
        message: String,
    },
}
