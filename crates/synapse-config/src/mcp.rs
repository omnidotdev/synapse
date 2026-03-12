use std::collections::HashMap;

use indexmap::IndexMap;
use secrecy::SecretString;
use serde::Deserialize;
use url::Url;

/// Top-level MCP configuration
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    /// MCP server configurations keyed by name
    #[serde(default)]
    pub servers: IndexMap<String, McpServerConfig>,
    /// Dynamic downstream connection cache settings
    #[serde(default)]
    pub cache: Option<McpCacheConfig>,
}

/// Configuration for a single MCP server
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    /// Server transport type
    #[serde(rename = "type")]
    pub server_type: McpServerType,
    /// Rate limit for this server
    #[serde(default)]
    pub rate_limit: Option<McpRateLimit>,
    /// Access control for this server
    #[serde(default)]
    pub access: Option<McpAccessConfig>,
    /// Per-tool rate limit overrides
    #[serde(default)]
    pub tool_rate_limits: HashMap<String, McpRateLimit>,
    /// Headers to insert on requests to this server
    #[serde(default)]
    pub headers: Vec<McpHeaderInsert>,
    /// Enable structured content responses
    #[serde(default)]
    pub structured_content: bool,
}

/// MCP server transport types
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpServerType {
    /// STDIO subprocess
    Stdio(StdioConfig),
    /// HTTP with SSE
    Sse(HttpConfig),
    /// HTTP with streamable protocol
    StreamableHttp(HttpConfig),
}

/// STDIO transport configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StdioConfig {
    /// Command to execute
    pub command: String,
    /// Command arguments
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// HTTP transport configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    /// Server URL
    pub url: Url,
    /// Authentication configuration
    #[serde(default)]
    pub auth: Option<McpAuthConfig>,
    /// TLS client configuration
    #[serde(default)]
    pub tls: Option<McpTlsConfig>,
}

/// MCP server authentication
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpAuthConfig {
    /// Static bearer token
    Token { token: SecretString },
    /// Forward the client's authorization header
    Forward,
}

/// TLS client configuration for MCP connections
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpTlsConfig {
    /// Path to CA certificate
    #[serde(default)]
    pub ca_cert: Option<String>,
    /// Path to client certificate
    #[serde(default)]
    pub client_cert: Option<String>,
    /// Path to client key
    #[serde(default)]
    pub client_key: Option<String>,
}

/// MCP rate limit configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpRateLimit {
    /// Maximum requests per window
    pub requests: u32,
    /// Window duration (e.g. "1m", "1h")
    pub window: String,
}

/// Access control for MCP servers/tools
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpAccessConfig {
    /// Allowed tool names (if set, only these tools are accessible)
    #[serde(default)]
    pub allow: Vec<String>,
    /// Denied tool names (if set, these tools are blocked)
    #[serde(default)]
    pub deny: Vec<String>,
}

/// Header to insert on MCP requests
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpHeaderInsert {
    /// Header name
    pub name: String,
    /// Header value
    pub value: String,
}

/// Cache configuration for dynamic MCP downstreams
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpCacheConfig {
    /// Maximum number of cached connections
    #[serde(default = "default_cache_max")]
    pub max_connections: u64,
    /// TTL in seconds
    #[serde(default = "default_cache_ttl")]
    pub ttl: u64,
}

#[allow(clippy::missing_const_for_fn)]
fn default_cache_max() -> u64 {
    100
}
#[allow(clippy::missing_const_for_fn)]
fn default_cache_ttl() -> u64 {
    300
}
