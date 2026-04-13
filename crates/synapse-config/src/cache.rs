use serde::Deserialize;
use url::Url;

/// Response cache configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseCacheConfig {
    /// Whether caching is enabled
    #[serde(default)]
    pub enabled: bool,
    /// Valkey connection URL
    pub url: Url,
    /// Default TTL in seconds for cached responses
    #[serde(default = "default_ttl_seconds")]
    pub ttl_seconds: u64,
    /// Key prefix in Valkey
    #[serde(default = "default_key_prefix")]
    pub key_prefix: String,
}

#[allow(clippy::missing_const_for_fn)]
fn default_ttl_seconds() -> u64 {
    3600
}

fn default_key_prefix() -> String {
    "synapse:cache".to_owned()
}
