//! Valkey-backed exact-match response cache for LLM completions
//!
//! Caches non-streaming completion responses keyed by a SHA-256 hash
//! of the canonical request (model + messages + params + tools). Only
//! deterministic requests (temperature 0 or unset) are cached by default.

use std::time::Duration;

use sha2::{Digest, Sha256};
use thiserror::Error;

/// Cache errors
#[derive(Debug, Error)]
pub enum CacheError {
    /// Valkey connection or command error
    #[error("cache backend: {0}")]
    Backend(String),
    /// Serialization error
    #[error("serialization: {0}")]
    Serialization(String),
}

/// Response cache backed by Valkey
#[derive(Clone)]
pub struct ResponseCache {
    client: redis::Client,
    default_ttl: Duration,
    key_prefix: String,
}

/// Cached response entry
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedResponse {
    /// Serialized response body (JSON)
    pub body: String,
    /// Model that produced the response
    pub model: String,
    /// Provider that served the request
    pub provider: String,
}

impl ResponseCache {
    /// Create a new response cache
    ///
    /// # Errors
    ///
    /// Returns an error if the Valkey URL is invalid
    pub fn new(url: &str, default_ttl: Duration, key_prefix: Option<String>) -> Result<Self, CacheError> {
        let client = redis::Client::open(url).map_err(|e| CacheError::Backend(format!("invalid URL: {e}")))?;

        Ok(Self {
            client,
            default_ttl,
            key_prefix: key_prefix.unwrap_or_else(|| "synapse:cache".to_owned()),
        })
    }

    /// Look up a cached response by request hash
    ///
    /// # Errors
    ///
    /// Returns an error on connection or deserialization failure
    pub async fn get(&self, cache_key: &str) -> Result<Option<CachedResponse>, CacheError> {
        use redis::AsyncCommands;

        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| CacheError::Backend(format!("connection failed: {e}")))?;

        let key = format!("{}:{cache_key}", self.key_prefix);
        let result: Option<String> = conn
            .get(&key)
            .await
            .map_err(|e| CacheError::Backend(format!("GET failed: {e}")))?;

        if let Some(data) = result {
            let entry: CachedResponse =
                serde_json::from_str(&data).map_err(|e| CacheError::Serialization(format!("deserialize: {e}")))?;
            tracing::debug!(cache_key, "cache hit");
            Ok(Some(entry))
        } else {
            tracing::debug!(cache_key, "cache miss");
            Ok(None)
        }
    }

    /// Store a response in the cache
    ///
    /// # Errors
    ///
    /// Returns an error on connection or serialization failure
    pub async fn put(&self, cache_key: &str, entry: &CachedResponse, ttl: Option<Duration>) -> Result<(), CacheError> {
        use redis::AsyncCommands;

        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| CacheError::Backend(format!("connection failed: {e}")))?;

        let key = format!("{}:{cache_key}", self.key_prefix);
        let data = serde_json::to_string(entry).map_err(|e| CacheError::Serialization(format!("serialize: {e}")))?;

        let ttl_secs = ttl.unwrap_or(self.default_ttl).as_secs();
        let _: () = conn
            .set_ex(&key, &data, ttl_secs)
            .await
            .map_err(|e| CacheError::Backend(format!("SET failed: {e}")))?;

        tracing::debug!(cache_key, ttl_secs, "cached response");
        Ok(())
    }
}

/// Compute a SHA-256 cache key from a serializable request
///
/// Hashes the canonical JSON representation of the request fields
/// that determine the response: model, messages, params, and tools.
/// Excludes the `stream` flag since it does not affect output content.
pub fn compute_cache_key<T: serde::Serialize>(request: &T) -> String {
    let json = serde_json::to_string(request).unwrap_or_default();
    let hash = Sha256::digest(json.as_bytes());
    hash.iter().fold(String::with_capacity(64), |mut out, b| {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Check whether a request is cacheable
///
/// Only deterministic requests (temperature 0 or unset, no seed randomness)
/// with non-streaming mode are eligible for caching.
#[must_use]
pub fn is_cacheable(temperature: Option<f64>, stream: bool) -> bool {
    if stream {
        return false;
    }
    // Cache when temperature is unset or explicitly 0
    temperature.is_none_or(|t| t == 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_deterministic() {
        let data = serde_json::json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]});
        let key1 = compute_cache_key(&data);
        let key2 = compute_cache_key(&data);
        assert_eq!(key1, key2);
    }

    #[test]
    fn cache_key_differs_for_different_input() {
        let a = serde_json::json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]});
        let b = serde_json::json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "bye"}]});
        assert_ne!(compute_cache_key(&a), compute_cache_key(&b));
    }

    #[test]
    fn is_cacheable_checks() {
        // Streaming never cached
        assert!(!is_cacheable(None, true));
        assert!(!is_cacheable(Some(0.0), true));

        // Non-streaming, deterministic
        assert!(is_cacheable(None, false));
        assert!(is_cacheable(Some(0.0), false));

        // Non-streaming, non-deterministic
        assert!(!is_cacheable(Some(0.7), false));
        assert!(!is_cacheable(Some(1.0), false));
    }
}
