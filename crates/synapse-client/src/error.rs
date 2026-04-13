/// Client-specific result type
pub type Result<T> = std::result::Result<T, SynapseClientError>;

/// Errors from the Synapse client
#[derive(Debug, thiserror::Error)]
pub enum SynapseClientError {
    /// HTTP transport error
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// Server returned an error response
    #[error("{status} {error_type}: {message}")]
    Api {
        /// HTTP status code
        status: u16,
        /// Error type identifier
        error_type: String,
        /// Human-readable error message
        message: String,
    },

    /// Failed to parse response
    #[error("failed to parse response: {0}")]
    Parse(String),

    /// Stream encountered an error
    #[error("stream error: {0}")]
    Stream(String),

    /// Invalid configuration
    #[error("invalid configuration: {0}")]
    Config(String),

    /// LLM provider error (embedded mode)
    #[cfg(feature = "embedded")]
    #[error("LLM error: {0}")]
    Llm(String),
}
