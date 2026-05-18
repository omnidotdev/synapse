use secrecy::SecretString;
use serde::Deserialize;
use url::Url;

/// API key authentication configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// Whether API key auth is enabled
    #[serde(default)]
    pub enabled: bool,

    /// URL of the synapse-api service
    pub api_url: Url,

    /// Shared secret for gateway-to-API calls
    pub gateway_secret: SecretString,

    /// Cache TTL in seconds for resolved API keys
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_seconds: u64,

    /// Maximum number of cached key resolutions
    #[serde(default = "default_cache_capacity")]
    pub cache_capacity: u64,

    /// Paths that skip authentication
    #[serde(default = "default_public_paths")]
    pub public_paths: Vec<String>,

    /// Skip TLS certificate verification for API calls (dev only)
    #[serde(default)]
    pub tls_skip_verify: bool,

    /// Gatekeeper vault settings for BYOK key resolution
    #[serde(default)]
    pub vault: Option<VaultConfig>,
}

/// Gatekeeper vault configuration for BYOK key resolution
///
/// When configured, BYOK provider keys are resolved from Gatekeeper's
/// vault instead of synapse-api's local database
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultConfig {
    /// Gatekeeper base URL
    pub url: String,

    /// Service-to-service API key for Gatekeeper
    pub service_key: SecretString,

    /// Cache TTL in seconds for vault key resolutions
    #[serde(default = "default_vault_cache_ttl")]
    pub cache_ttl_seconds: u64,

    /// Maximum number of cached vault key resolutions
    #[serde(default = "default_vault_cache_capacity")]
    pub cache_capacity: u64,
}

#[allow(clippy::missing_const_for_fn)]
fn default_vault_cache_ttl() -> u64 {
    300
}

#[allow(clippy::missing_const_for_fn)]
fn default_vault_cache_capacity() -> u64 {
    10_000
}

#[allow(clippy::missing_const_for_fn)]
fn default_cache_ttl() -> u64 {
    30
}

#[allow(clippy::missing_const_for_fn)]
fn default_cache_capacity() -> u64 {
    10_000
}

fn default_public_paths() -> Vec<String> {
    vec!["/health".to_string()]
}
