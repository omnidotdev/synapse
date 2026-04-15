use serde::Deserialize;

/// Configuration for extracting client IP addresses
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientIpConfig {
    /// Number of trusted proxy hops for X-Forwarded-For
    #[serde(default)]
    pub trusted_hops: Option<usize>,
}
