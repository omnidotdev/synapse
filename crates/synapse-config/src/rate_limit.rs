use serde::Deserialize;
use url::Url;

/// Rate limiting configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitConfig {
    /// Storage backend
    #[serde(default)]
    pub storage: RateLimitStorage,
    /// Global rate limit (all requests)
    #[serde(default)]
    pub global: Option<RequestRateLimit>,
    /// Per-IP rate limit
    #[serde(default)]
    pub per_ip: Option<RequestRateLimit>,
    /// Token-based rate limits for LLM
    #[serde(default)]
    pub tokens: Option<TokenRateLimitConfig>,
}

/// Rate limit storage backend
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RateLimitStorage {
    /// In-memory storage (single instance only)
    #[default]
    Memory,
    /// Cache-backed storage (distributed)
    Cache(CacheConfig),
}

/// Cache configuration for rate limiting
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    /// Cache connection URL
    pub url: Url,
    /// Connection pool size
    #[serde(default = "default_pool_size")]
    pub pool_size: usize,
    /// Connection timeout in seconds
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout: u64,
    /// TLS configuration
    #[serde(default)]
    pub tls: Option<CacheTlsConfig>,
}

/// Cache TLS configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheTlsConfig {
    /// CA certificate path
    #[serde(default)]
    pub ca_cert: Option<String>,
    /// Client certificate path
    #[serde(default)]
    pub client_cert: Option<String>,
    /// Client key path
    #[serde(default)]
    pub client_key: Option<String>,
}

/// Request-based rate limit
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestRateLimit {
    /// Maximum requests per window
    pub requests: u32,
    /// Window duration (e.g. "1m", "1h")
    pub window: String,
}

/// Token-based rate limit configuration for LLM
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenRateLimitConfig {
    /// Default token limit per client per window
    pub default: TokenRateLimit,
    /// Per-group overrides
    #[serde(default)]
    pub groups: std::collections::HashMap<String, TokenRateLimit>,
}

/// Token rate limit
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenRateLimit {
    /// Maximum tokens per window
    pub tokens: u64,
    /// Window duration (e.g. "1m", "1h")
    pub window: String,
}

#[allow(clippy::missing_const_for_fn)]
fn default_pool_size() -> usize {
    10
}
#[allow(clippy::missing_const_for_fn)]
fn default_connect_timeout() -> u64 {
    5
}
