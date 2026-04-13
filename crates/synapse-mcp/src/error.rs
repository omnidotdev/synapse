use http::StatusCode;
use synapse_core::HttpError;
use thiserror::Error;

/// MCP subsystem errors
#[derive(Debug, Error)]
pub enum McpError {
    /// Requested server does not exist in configuration
    #[error("server not found: {server}")]
    ServerNotFound { server: String },

    /// Tool not found on any connected server
    #[error("tool not found: {tool}")]
    ToolNotFound { tool: String },

    /// Client lacks access to the requested tool
    #[error("access denied to tool: {tool}")]
    AccessDenied { tool: String },

    /// Transport-level connection or communication error
    #[error("transport error: {0}")]
    Transport(String),

    /// Tool execution returned an error
    #[error("tool execution failed: {0}")]
    Execution(String),

    /// Rate limit exceeded for this server or tool
    #[error("rate limit exceeded")]
    RateLimited { retry_after: u64 },

    /// Internal error
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl HttpError for McpError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::ServerNotFound { .. } | Self::ToolNotFound { .. } => StatusCode::NOT_FOUND,
            Self::AccessDenied { .. } => StatusCode::FORBIDDEN,
            Self::Transport(_) => StatusCode::BAD_GATEWAY,
            Self::Execution(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
        }
    }

    fn error_type(&self) -> &str {
        match self {
            Self::ServerNotFound { .. } | Self::ToolNotFound { .. } => "not_found",
            Self::AccessDenied { .. } => "access_denied",
            Self::Transport(_) => "transport_error",
            Self::Execution(_) => "execution_error",
            Self::RateLimited { .. } => "rate_limited",
            Self::Internal(_) => "internal_error",
        }
    }

    fn client_message(&self) -> String {
        match self {
            Self::ServerNotFound { server } => format!("MCP server not found: {server}"),
            Self::ToolNotFound { tool } => format!("tool not found: {tool}"),
            Self::AccessDenied { tool } => format!("access denied to tool: {tool}"),
            Self::Transport(_) => "failed to communicate with MCP server".to_string(),
            Self::Execution(msg) => format!("tool execution failed: {msg}"),
            Self::RateLimited { retry_after } => {
                format!("rate limit exceeded, retry after {retry_after}s")
            }
            Self::Internal(_) => "internal server error".to_string(),
        }
    }
}
