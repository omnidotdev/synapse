use std::time::Duration;

use serde::Deserialize;
use url::Url;

/// `OAuth2` authentication configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthConfig {
    /// JWKS endpoint URL for fetching signing keys
    pub jwks_url: Url,
    /// How often to poll for new JWKS keys (in seconds, default 300)
    #[serde(default = "default_poll_interval")]
    pub poll_interval: u64,
    /// Expected issuer claim value
    #[serde(default)]
    pub issuer: Option<String>,
    /// Expected audience claim value(s)
    #[serde(default)]
    pub audience: Option<Vec<String>>,
    /// Protected resource metadata configuration
    #[serde(default)]
    pub protected_resource: Option<ProtectedResourceConfig>,
}

/// Protected resource metadata configuration (RFC 9728)
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedResourceConfig {
    /// Resource identifier
    pub resource: Url,
    /// Authorization servers
    #[serde(default)]
    pub authorization_servers: Vec<Url>,
    /// Supported scopes
    #[serde(default)]
    pub scopes_supported: Vec<String>,
    /// Supported bearer methods
    #[serde(default)]
    pub bearer_methods_supported: Vec<String>,
}

impl OAuthConfig {
    /// Get poll interval as Duration
    pub const fn poll_interval_duration(&self) -> Duration {
        Duration::from_secs(self.poll_interval)
    }
}

#[allow(clippy::missing_const_for_fn)]
fn default_poll_interval() -> u64 {
    300
}
