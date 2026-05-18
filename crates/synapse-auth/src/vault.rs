use std::sync::Arc;
use std::time::Duration;

use mini_moka::sync::Cache;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

/// Default cache time-to-live (5 minutes)
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(300);

/// Default maximum cache capacity
const DEFAULT_CACHE_CAPACITY: u64 = 10_000;

/// A provider key resolved from Gatekeeper's vault
#[derive(Clone, Debug)]
pub struct VaultKey {
    /// Provider name (e.g. "anthropic", "openai")
    pub provider: String,
    /// Decrypted provider API key
    pub key: SecretString,
    /// Optional model override from vault metadata
    pub model_override: Option<String>,
}

/// Vault resolution errors
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// HTTP or network-level failure
    #[error("network error: {0}")]
    Network(String),

    /// Failed to parse the vault response
    #[error("parse error: {0}")]
    Parse(String),

    /// Service key is invalid or expired
    #[error("unauthorized")]
    Unauthorized,

    /// Gatekeeper vault feature is not configured
    #[error("vault not configured")]
    VaultNotConfigured,

    /// Non-success response from Gatekeeper
    #[error("API error ({status}): {message}")]
    Api {
        /// HTTP status code
        status: u16,
        /// Error message
        message: String,
    },
}

/// Raw response shape from `POST /api/vault/resolve`
#[derive(Deserialize)]
struct VaultResolveResponse {
    provider: String,
    key: String,
    model_override: Option<String>,
}

/// Client for resolving user provider keys from Gatekeeper's vault
///
/// Wraps HTTP calls to `POST /api/vault/resolve` with an in-memory
/// cache keyed by `{user_id}:{provider}`.
#[derive(Clone)]
pub struct VaultClient {
    http: reqwest::Client,
    base_url: String,
    service_key: SecretString,
    cache: Cache<String, Arc<VaultKey>>,
}

impl VaultClient {
    /// Create a new vault client
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be built
    pub fn new(
        base_url: impl Into<String>,
        service_key: SecretString,
        cache_ttl: Option<Duration>,
        cache_capacity: Option<u64>,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(5)).build()?;

        let cache = Cache::builder()
            .time_to_live(cache_ttl.unwrap_or(DEFAULT_CACHE_TTL))
            .max_capacity(cache_capacity.unwrap_or(DEFAULT_CACHE_CAPACITY))
            .build();

        Ok(Self {
            http,
            base_url: base_url.into(),
            service_key,
            cache,
        })
    }

    /// Resolve a user's provider key from Gatekeeper's vault
    ///
    /// Returns `Ok(None)` when the user has no key stored for the
    /// requested provider. Results are cached for the configured TTL.
    ///
    /// # Errors
    ///
    /// Returns `VaultError` on network failures, auth issues, or
    /// unexpected API responses
    pub async fn resolve(&self, user_id: &str, provider: &str) -> Result<Option<Arc<VaultKey>>, VaultError> {
        let cache_key = format!("{user_id}:{provider}");

        if let Some(cached) = self.cache.get(&cache_key) {
            return Ok(Some(cached));
        }

        let url = format!("{}/api/vault/resolve", self.base_url);

        let response = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.service_key.expose_secret()))
            .header("X-User-Id", user_id)
            .json(&serde_json::json!({
                "provider": provider,
            }))
            .send()
            .await
            .map_err(|e| VaultError::Network(e.to_string()))?;

        let status = response.status().as_u16();

        match status {
            404 => return Ok(None),
            401 => return Err(VaultError::Unauthorized),
            503 => return Err(VaultError::VaultNotConfigured),
            s if !(200..300).contains(&s) => {
                let message = response.text().await.unwrap_or_default();
                return Err(VaultError::Api { status: s, message });
            }
            _ => {}
        }

        let body: VaultResolveResponse = response.json().await.map_err(|e| VaultError::Parse(e.to_string()))?;

        let vault_key = Arc::new(VaultKey {
            provider: body.provider,
            key: SecretString::from(body.key),
            model_override: body.model_override,
        });

        self.cache.insert(cache_key, Arc::clone(&vault_key));

        tracing::debug!(user_id, provider, "resolved vault key from gatekeeper");

        Ok(Some(vault_key))
    }

    /// Resolve keys for multiple providers, skipping any that are not found
    ///
    /// Errors for individual providers are logged and skipped so that
    /// a single missing key does not block the rest.
    pub async fn resolve_all(&self, user_id: &str, providers: &[&str]) -> Vec<Arc<VaultKey>> {
        let mut keys = Vec::with_capacity(providers.len());

        for provider in providers {
            match self.resolve(user_id, provider).await {
                Ok(Some(key)) => keys.push(key),
                Ok(None) => {
                    tracing::debug!(user_id, provider, "no vault key found");
                }
                Err(e) => {
                    tracing::warn!(user_id, provider, error = %e, "failed to resolve vault key");
                }
            }
        }

        keys
    }

    /// Evict a cached vault key (e.g. after a Vortex invalidation event)
    pub fn evict(&self, user_id: &str, provider: &str) {
        let cache_key = format!("{user_id}:{provider}");
        self.cache.invalidate(&cache_key);
        tracing::debug!(user_id, provider, "evicted vault cache entry");
    }
}
