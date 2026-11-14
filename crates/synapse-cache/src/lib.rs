//! Valkey-backed exact-match response cache for LLM completions
//!
//! Caches non-streaming completion responses keyed by a SHA-256 hash of the
//! caller's [`CacheScope`] plus the canonical request (model, messages,
//! params, tools). A [`CacheKey`] can only be built from a scope, so an entry
//! is never shared between tenants, billing modes, or API keys.
//!
//! Caching is opt-in per request: a caller asks for it with the
//! [`OPT_IN_HEADER`] header. Current models are nondeterministic even at
//! temperature 0, and products expect a fresh answer on "regenerate", so an
//! identical request is never answered from the cache unless the caller said
//! a repeated answer is acceptable.

use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Request header a caller sets to opt a request into the response cache
pub const OPT_IN_HEADER: &str = "x-synapse-cache";

/// Version of the key derivation, part of every key so a change to it
/// orphans every entry written under the previous derivation
const KEY_VERSION: &str = "v2";

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

/// How the scoped caller pays for the response
///
/// Part of every key: a response served on the caller's own provider key
/// (BYOK) must never answer a managed request, and the reverse
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheBillingMode {
    /// Omni-managed provider keys, metered and charged
    Managed,
    /// Caller's own provider keys
    Byok,
    /// No billing identity (billing disabled for this request)
    Unbilled,
}

/// The tenant a cached response belongs to
///
/// Built from the caller's resolved identity: the product (`app_id`), the
/// billing entity, the billing mode, and the API key or client that made the
/// request. Two requests share a cache entry only when every field matches
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheScope {
    app_id: String,
    entity_type: String,
    entity_id: String,
    billing_mode: CacheBillingMode,
    principal: Option<String>,
}

impl CacheScope {
    /// Scope a cache entry to a tenant
    ///
    /// `principal` narrows the scope to one credential within the entity
    /// (e.g. the API key id). Returns `None` when the entity is unidentified,
    /// so an anonymous request can never be cached
    #[must_use]
    pub fn new(
        app_id: impl Into<String>,
        entity_type: impl Into<String>,
        entity_id: impl Into<String>,
        billing_mode: CacheBillingMode,
        principal: Option<String>,
    ) -> Option<Self> {
        let entity_type = entity_type.into();
        let entity_id = entity_id.into();
        if entity_type.is_empty() || entity_id.is_empty() {
            return None;
        }

        Some(Self {
            app_id: app_id.into(),
            entity_type,
            entity_id,
            billing_mode,
            principal: principal.filter(|p| !p.is_empty()),
        })
    }
}

/// A tenant-scoped cache key
///
/// Only constructible from a [`CacheScope`], so an unscoped key cannot be
/// built and the cache cannot be read or written without one
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey(String);

impl CacheKey {
    /// Derive the key for a request made by `scope`
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be serialized (never falls back
    /// to a shared key)
    pub fn new<T: Serialize>(scope: &CacheScope, request: &T) -> Result<Self, CacheError> {
        #[derive(Serialize)]
        struct Keyed<'a, T> {
            version: &'static str,
            scope: &'a CacheScope,
            request: &'a T,
        }

        let canonical = serde_json::to_vec(&Keyed {
            version: KEY_VERSION,
            scope,
            request,
        })
        .map_err(|e| CacheError::Serialization(format!("cache key: {e}")))?;

        let hash = Sha256::digest(&canonical);
        let hex = hash.iter().fold(String::with_capacity(64), |mut out, b| {
            use std::fmt::Write;
            let _ = write!(out, "{b:02x}");
            out
        });

        Ok(Self(format!("{KEY_VERSION}:{hex}")))
    }

    /// The key as stored (without the configured prefix)
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Response cache backed by Valkey
#[derive(Clone)]
pub struct ResponseCache {
    backend: Backend,
    default_ttl: Duration,
    key_prefix: String,
}

#[derive(Clone)]
enum Backend {
    Valkey(Box<redis::Client>),
    #[cfg(feature = "test-util")]
    Memory(MemoryStore),
}

/// In-process store for tests: key to (serialized entry, expiry)
#[cfg(feature = "test-util")]
type MemoryStore = std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>>;

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
            backend: Backend::Valkey(Box::new(client)),
            default_ttl,
            key_prefix: key_prefix.unwrap_or_else(|| "synapse:cache".to_owned()),
        })
    }

    /// Create an in-process cache for tests
    #[cfg(feature = "test-util")]
    #[must_use]
    pub fn in_memory(default_ttl: Duration) -> Self {
        Self {
            backend: Backend::Memory(std::sync::Arc::default()),
            default_ttl,
            key_prefix: "synapse:cache".to_owned(),
        }
    }

    /// Look up a cached response
    ///
    /// # Errors
    ///
    /// Returns an error on connection or deserialization failure
    pub async fn get(&self, cache_key: &CacheKey) -> Result<Option<CachedResponse>, CacheError> {
        let key = format!("{}:{}", self.key_prefix, cache_key.as_str());

        let result: Option<String> = match &self.backend {
            Backend::Valkey(client) => {
                use redis::AsyncCommands;

                let mut conn = client
                    .get_multiplexed_async_connection()
                    .await
                    .map_err(|e| CacheError::Backend(format!("connection failed: {e}")))?;
                conn.get(&key)
                    .await
                    .map_err(|e| CacheError::Backend(format!("GET failed: {e}")))?
            }
            #[cfg(feature = "test-util")]
            Backend::Memory(map) => {
                let map = map.lock().map_err(|e| CacheError::Backend(e.to_string()))?;
                map.get(&key)
                    .filter(|(_, expires)| *expires > std::time::Instant::now())
                    .map(|(data, _)| data.clone())
            }
        };

        let cache_key = cache_key.as_str();
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
    pub async fn put(
        &self,
        cache_key: &CacheKey,
        entry: &CachedResponse,
        ttl: Option<Duration>,
    ) -> Result<(), CacheError> {
        let key = format!("{}:{}", self.key_prefix, cache_key.as_str());
        let data = serde_json::to_string(entry).map_err(|e| CacheError::Serialization(format!("serialize: {e}")))?;
        let ttl = ttl.unwrap_or(self.default_ttl);

        match &self.backend {
            Backend::Valkey(client) => {
                use redis::AsyncCommands;

                let mut conn = client
                    .get_multiplexed_async_connection()
                    .await
                    .map_err(|e| CacheError::Backend(format!("connection failed: {e}")))?;
                let _: () = conn
                    .set_ex(&key, &data, ttl.as_secs())
                    .await
                    .map_err(|e| CacheError::Backend(format!("SET failed: {e}")))?;
            }
            #[cfg(feature = "test-util")]
            Backend::Memory(map) => {
                let mut map = map.lock().map_err(|e| CacheError::Backend(e.to_string()))?;
                map.insert(key, (data, std::time::Instant::now() + ttl));
            }
        }

        tracing::debug!(
            cache_key = cache_key.as_str(),
            ttl_secs = ttl.as_secs(),
            "cached response"
        );
        Ok(())
    }
}

/// Whether an [`OPT_IN_HEADER`] value opts the request into caching
#[must_use]
pub fn opt_in_requested(header_value: Option<&str>) -> bool {
    header_value.is_some_and(|v| {
        let v = v.trim();
        v == "1" || v.eq_ignore_ascii_case("true")
    })
}

/// Check whether a request is cacheable
///
/// Only non-streaming requests whose caller explicitly opted in are eligible.
/// Temperature is deliberately not a signal: temperature 0 is still
/// nondeterministic on current models, some models reject sampling
/// parameters entirely, and callers retry or regenerate at the same settings
/// expecting a fresh answer
#[must_use]
pub const fn is_cacheable(opted_in: bool, stream: bool) -> bool {
    opted_in && !stream
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(entity_id: &str, mode: CacheBillingMode) -> CacheScope {
        CacheScope::new("facet", "user", entity_id, mode, Some("key_1".to_owned())).unwrap()
    }

    fn request(content: &str) -> serde_json::Value {
        serde_json::json!({"model": "gpt-4o", "messages": [{"role": "user", "content": content}]})
    }

    #[test]
    fn cache_key_deterministic() {
        let s = scope("usr_1", CacheBillingMode::Managed);
        assert_eq!(
            CacheKey::new(&s, &request("hi")).unwrap(),
            CacheKey::new(&s, &request("hi")).unwrap()
        );
    }

    #[test]
    fn cache_key_differs_for_different_input() {
        let s = scope("usr_1", CacheBillingMode::Managed);
        assert_ne!(
            CacheKey::new(&s, &request("hi")).unwrap(),
            CacheKey::new(&s, &request("bye")).unwrap()
        );
    }

    #[test]
    fn cache_key_differs_per_tenant_mode_and_credential() {
        let req = request("hi");
        let base = CacheKey::new(&scope("usr_1", CacheBillingMode::Managed), &req).unwrap();

        let other_entity = CacheKey::new(&scope("usr_2", CacheBillingMode::Managed), &req).unwrap();
        let byok = CacheKey::new(&scope("usr_1", CacheBillingMode::Byok), &req).unwrap();
        let other_app = CacheScope::new(
            "myfi",
            "user",
            "usr_1",
            CacheBillingMode::Managed,
            Some("key_1".to_owned()),
        );
        let other_app = CacheKey::new(&other_app.unwrap(), &req).unwrap();
        let other_type = CacheScope::new(
            "facet",
            "organization",
            "usr_1",
            CacheBillingMode::Managed,
            Some("key_1".to_owned()),
        );
        let other_type = CacheKey::new(&other_type.unwrap(), &req).unwrap();
        let other_key = CacheScope::new(
            "facet",
            "user",
            "usr_1",
            CacheBillingMode::Managed,
            Some("key_2".to_owned()),
        );
        let other_key = CacheKey::new(&other_key.unwrap(), &req).unwrap();

        for other in [other_entity, byok, other_app, other_type, other_key] {
            assert_ne!(base, other);
        }
    }

    #[test]
    fn unidentified_entity_has_no_scope() {
        assert!(CacheScope::new("facet", "user", "", CacheBillingMode::Managed, None).is_none());
        assert!(CacheScope::new("facet", "", "usr_1", CacheBillingMode::Managed, None).is_none());
    }

    #[test]
    fn keys_carry_the_derivation_version() {
        let key = CacheKey::new(&scope("usr_1", CacheBillingMode::Managed), &request("hi")).unwrap();
        assert!(key.as_str().starts_with("v2:"));
    }

    #[test]
    fn is_cacheable_requires_opt_in_and_no_streaming() {
        assert!(is_cacheable(true, false));
        assert!(!is_cacheable(true, true));
        assert!(!is_cacheable(false, false));
        assert!(!is_cacheable(false, true));
    }

    #[test]
    fn opt_in_header_values() {
        assert!(opt_in_requested(Some("true")));
        assert!(opt_in_requested(Some(" TRUE ")));
        assert!(opt_in_requested(Some("1")));
        assert!(!opt_in_requested(None));
        assert!(!opt_in_requested(Some("")));
        assert!(!opt_in_requested(Some("false")));
        assert!(!opt_in_requested(Some("0")));
    }
}
