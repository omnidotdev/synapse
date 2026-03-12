use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use mini_moka::sync::Cache;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::AuthError;

/// Plan-based rate limits from synapse-api
///
/// Token limits use `i64` because `-1` represents unlimited.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimits {
    /// Maximum requests per minute
    pub requests_per_minute: u32,
    /// Maximum tokens per day (`-1` = unlimited)
    pub tokens_per_day: i64,
    /// Maximum tokens per month (`-1` = unlimited)
    pub tokens_per_month: i64,
}

/// Resolved API key context from synapse-api
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedKey {
    /// User ID that owns this key
    pub user_id: String,
    /// Workspace scope (optional)
    pub workspace_id: Option<String>,
    /// The API key record ID
    pub api_key_id: String,
    /// Key mode (BYOK or managed)
    pub mode: KeyMode,
    /// User's plan tier
    pub plan: String,
    /// Plan-based rate limits
    #[serde(default)]
    pub rate_limits: Option<RateLimits>,
    /// Decrypted provider keys (only for BYOK mode)
    #[serde(default)]
    pub provider_keys: Vec<ProviderKeyRef>,
}

/// API key billing mode
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KeyMode {
    /// User provides their own provider keys
    Byok,
    /// Omni-managed provider keys with billing
    Managed,
    /// Manually created key (treated as managed for billing)
    Manual,
}

/// Reference to a decrypted provider key
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderKeyRef {
    /// Provider name (e.g. "openai", "anthropic")
    pub provider: String,
    /// Decrypted API key
    pub decrypted_key: String,
}

/// Resolves API keys by calling synapse-api with caching
#[derive(Clone)]
pub struct ApiKeyResolver {
    http: reqwest::Client,
    api_url: url::Url,
    gateway_secret: SecretString,
    cache: Cache<String, Arc<ResolvedKey>>,
}

impl ApiKeyResolver {
    /// Create a new resolver
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be built
    pub fn new(
        api_url: url::Url,
        gateway_secret: SecretString,
        cache_ttl: Duration,
        cache_capacity: u64,
        tls_skip_verify: bool,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .danger_accept_invalid_certs(tls_skip_verify)
            .build()?;

        let cache = Cache::builder()
            .time_to_live(cache_ttl)
            .max_capacity(cache_capacity)
            .build();

        Ok(Self {
            http,
            api_url,
            gateway_secret,
            cache,
        })
    }

    /// Resolve an API key to user context
    ///
    /// Results are cached for the configured TTL.
    ///
    /// # Errors
    ///
    /// Returns `AuthError` if the key is invalid, expired, or the API is unreachable
    pub async fn resolve(&self, raw_key: &str) -> Result<Arc<ResolvedKey>, AuthError> {
        let cache_key = sha256_hex(raw_key);

        if let Some(cached) = self.cache.get(&cache_key) {
            return Ok(cached);
        }

        let url = self
            .api_url
            .join("/internal/resolve-key")
            .map_err(|e| AuthError::ApiError {
                status: 0,
                message: e.to_string(),
            })?;

        let response = self
            .http
            .post(url)
            .header("X-Gateway-Secret", self.gateway_secret.expose_secret())
            .json(&serde_json::json!({ "key": raw_key }))
            .send()
            .await?;

        let status = response.status().as_u16();

        if status == 404 {
            return Err(AuthError::InvalidKey);
        }

        if !response.status().is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(AuthError::ApiError { status, message });
        }

        let resolved: ResolvedKey = response.json().await.map_err(|e| AuthError::ApiError {
            status: 0,
            message: format!("failed to parse response: {e}"),
        })?;

        let resolved = Arc::new(resolved);
        self.cache.insert(cache_key, Arc::clone(&resolved));

        Ok(resolved)
    }

    /// Remove a cached key resolution (e.g. after revocation)
    pub fn invalidate(&self, raw_key: &str) {
        let cache_key = sha256_hex(raw_key);
        self.cache.invalidate(&cache_key);
    }
}

/// Compute the SHA-256 hex digest of a string
fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        // Writing hex to a String is infallible
        write!(hex, "{byte:02x}").unwrap();
    }
    hex
}
