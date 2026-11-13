use std::collections::HashSet;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use mini_moka::sync::Cache;
use synapse_core::{BillingIdentity, TokenLimits};

/// Cached result of an entitlement check
#[derive(Debug, Clone)]
pub struct CachedEntitlement {
    /// Whether access is granted
    pub has_access: bool,
    /// Entitlement version for cache invalidation
    #[allow(dead_code)]
    pub version: Option<u64>,
}

/// Cached result of a usage check
#[derive(Debug, Clone)]
pub struct CachedUsageCheck {
    /// Whether additional usage is within limits
    pub allowed: bool,
}

/// Cached per-request token limits resolved from entitlements
#[derive(Debug, Clone)]
pub struct CachedTokenLimits {
    /// Resolved token limits
    pub limits: TokenLimits,
}

/// The product and entity a cached billing decision belongs to
///
/// One Omni user is the same entity across every product, so the product
/// (`app_id`) is part of every cache key: a decision cached for one product
/// must never answer a check for another
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scope<'a> {
    /// Product the decision was made for (the Aether `appId`)
    pub app_id: &'a str,
    /// Entity type (e.g. "user")
    pub entity_type: &'a str,
    /// Entity identifier
    pub entity_id: &'a str,
}

impl<'a> Scope<'a> {
    /// Scope of a request's resolved billing identity
    pub fn of(identity: &'a BillingIdentity) -> Self {
        Self {
            app_id: &identity.app_id,
            entity_type: &identity.entity_type,
            entity_id: &identity.entity_id,
        }
    }

    fn key(&self, suffix: &str) -> String {
        format!("{}:{}:{}:{suffix}", self.app_id, self.entity_type, self.entity_id)
    }
}

/// In-memory TTL cache for entitlement and usage checks
///
/// Reduces load on Aether by caching results for a configurable duration.
/// Positive results use the full TTL; negative usage results use a shorter
/// TTL (5s) to prevent thundering herd on rate-limited users while still
/// allowing limit resets to propagate quickly.
#[derive(Clone)]
pub struct EntitlementCache {
    entitlements: Cache<String, CachedEntitlement>,
    usage: Cache<String, CachedUsageCheck>,
    usage_denied: Cache<String, CachedUsageCheck>,
    token_limits: Cache<String, CachedTokenLimits>,
    /// Products seen so far, so an invalidation that does not name a product
    /// can clear the entity in every product (bounded by the product count)
    apps: Arc<RwLock<HashSet<String>>>,
}

/// Short TTL for negative usage results to prevent thundering herd
const DENIED_USAGE_TTL: Duration = Duration::from_secs(5);

const TOKEN_LIMITS_SUFFIX: &str = "token_limits";

impl EntitlementCache {
    /// Create a new cache with the given TTL in seconds
    pub fn new(ttl_secs: u64) -> Self {
        let ttl = Duration::from_secs(ttl_secs);

        Self {
            entitlements: Cache::builder().max_capacity(10_000).time_to_live(ttl).build(),
            usage: Cache::builder().max_capacity(10_000).time_to_live(ttl).build(),
            usage_denied: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(DENIED_USAGE_TTL)
                .build(),
            token_limits: Cache::builder().max_capacity(10_000).time_to_live(ttl).build(),
            apps: Arc::default(),
        }
    }

    /// Look up a cached entitlement check
    pub fn get_entitlement(&self, scope: &Scope<'_>, feature_key: &str) -> Option<CachedEntitlement> {
        self.entitlements.get(&scope.key(feature_key))
    }

    /// Cache an entitlement check result
    pub fn put_entitlement(&self, scope: &Scope<'_>, feature_key: &str, result: CachedEntitlement) {
        self.remember(scope.app_id);
        self.entitlements.insert(scope.key(feature_key), result);
    }

    /// Look up a cached usage check (checks both positive and negative caches)
    pub fn get_usage(&self, scope: &Scope<'_>, meter_key: &str) -> Option<CachedUsageCheck> {
        let key = scope.key(meter_key);
        self.usage.get(&key).or_else(|| self.usage_denied.get(&key))
    }

    /// Cache a usage check result.
    /// Positive results use the full TTL; negative results use a short TTL
    /// to prevent thundering herd while allowing limit resets to propagate.
    pub fn put_usage(&self, scope: &Scope<'_>, meter_key: &str, result: CachedUsageCheck) {
        self.remember(scope.app_id);
        let key = scope.key(meter_key);
        if result.allowed {
            self.usage.insert(key, result);
        } else {
            self.usage_denied.insert(key, result);
        }
    }

    /// Look up cached per-request token limits
    pub fn get_token_limits(&self, scope: &Scope<'_>) -> Option<CachedTokenLimits> {
        self.token_limits.get(&scope.key(TOKEN_LIMITS_SUFFIX))
    }

    /// Cache per-request token limits
    pub fn put_token_limits(&self, scope: &Scope<'_>, result: CachedTokenLimits) {
        self.remember(scope.app_id);
        self.token_limits.insert(scope.key(TOKEN_LIMITS_SUFFIX), result);
    }

    /// Invalidate cached per-request token limits (used by entitlement webhooks)
    ///
    /// `app_id: None` clears the entity in every product
    pub fn invalidate_token_limits(&self, app_id: Option<&str>, entity_type: &str, entity_id: &str) {
        for key in self.keys_for(app_id, entity_type, entity_id, TOKEN_LIMITS_SUFFIX) {
            self.token_limits.invalidate(&key);
        }
    }

    /// Invalidate a cached entitlement
    ///
    /// `app_id: None` clears the entity in every product
    pub fn invalidate_entitlement(&self, app_id: Option<&str>, entity_type: &str, entity_id: &str, feature_key: &str) {
        for key in self.keys_for(app_id, entity_type, entity_id, feature_key) {
            self.entitlements.invalidate(&key);
        }
    }

    /// Invalidate a cached usage check (both positive and negative)
    ///
    /// `app_id: None` clears the entity in every product
    pub fn invalidate_usage(&self, app_id: Option<&str>, entity_type: &str, entity_id: &str, meter_key: &str) {
        for key in self.keys_for(app_id, entity_type, entity_id, meter_key) {
            self.usage.invalidate(&key);
            self.usage_denied.invalidate(&key);
        }
    }

    fn remember(&self, app_id: &str) {
        let known = self
            .apps
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(app_id);
        if !known {
            self.apps
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(app_id.to_owned());
        }
    }

    /// Cache keys to invalidate: one product, or the entity in every product seen
    fn keys_for(&self, app_id: Option<&str>, entity_type: &str, entity_id: &str, suffix: &str) -> Vec<String> {
        let key = |app_id: &str| {
            Scope {
                app_id,
                entity_type,
                entity_id,
            }
            .key(suffix)
        };

        // A poisoned lock still holds a valid set of names, so recover it
        // rather than skip invalidation and leave a stale grant behind
        app_id.map_or_else(
            || {
                let apps = self.apps.read().unwrap_or_else(PoisonError::into_inner);
                apps.iter().map(|app_id| key(app_id)).collect()
            },
            |app_id| vec![key(app_id)],
        )
    }
}

impl std::fmt::Debug for EntitlementCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntitlementCache").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYNAPSE: Scope<'static> = Scope {
        app_id: "synapse",
        entity_type: "user",
        entity_id: "usr_1",
    };

    const FACET: Scope<'static> = Scope {
        app_id: "facet",
        entity_type: "user",
        entity_id: "usr_1",
    };

    const fn granted() -> CachedEntitlement {
        CachedEntitlement {
            has_access: true,
            version: Some(1),
        }
    }

    const fn limits(max_input_tokens: usize) -> CachedTokenLimits {
        CachedTokenLimits {
            limits: TokenLimits {
                max_input_tokens,
                max_output_tokens: 4096,
            },
        }
    }

    #[test]
    fn cache_miss_returns_none() {
        let cache = EntitlementCache::new(60);
        assert!(cache.get_entitlement(&SYNAPSE, "api_access").is_none());
        assert!(cache.get_usage(&SYNAPSE, "input_tokens").is_none());
    }

    #[test]
    fn cache_hit_returns_value() {
        let cache = EntitlementCache::new(60);
        cache.put_entitlement(&SYNAPSE, "api_access", granted());

        let result = cache.get_entitlement(&SYNAPSE, "api_access");
        assert!(result.is_some_and(|r| r.has_access));
    }

    #[test]
    fn cache_usage_hit() {
        let cache = EntitlementCache::new(60);
        cache.put_usage(&SYNAPSE, "input_tokens", CachedUsageCheck { allowed: true });

        let result = cache.get_usage(&SYNAPSE, "input_tokens");
        assert!(result.is_some_and(|r| r.allowed));
    }

    #[test]
    fn cache_token_limits_hit() {
        let cache = EntitlementCache::new(60);
        cache.put_token_limits(&SYNAPSE, limits(8192));

        let result = cache.get_token_limits(&SYNAPSE).map(|c| c.limits);
        assert_eq!(result.as_ref().map(|l| l.max_input_tokens), Some(8192));
        assert_eq!(result.as_ref().map(|l| l.max_output_tokens), Some(4096));
    }

    #[test]
    fn cache_token_limits_miss() {
        let cache = EntitlementCache::new(60);
        assert!(cache.get_token_limits(&SYNAPSE).is_none());
    }

    #[test]
    fn same_entity_is_isolated_per_product() {
        // One Omni user exists in many products; a grant cached for one
        // product must never answer a check for another
        let cache = EntitlementCache::new(60);
        cache.put_entitlement(&SYNAPSE, "api_access", granted());
        cache.put_usage(&SYNAPSE, "requests", CachedUsageCheck { allowed: true });
        cache.put_token_limits(&SYNAPSE, limits(200_000));

        assert!(cache.get_entitlement(&FACET, "api_access").is_none());
        assert!(cache.get_usage(&FACET, "requests").is_none());
        assert!(cache.get_token_limits(&FACET).is_none());
    }

    #[test]
    fn invalidate_clears_specific_entries() {
        let cache = EntitlementCache::new(60);
        cache.put_entitlement(&SYNAPSE, "api_access", granted());
        cache.put_usage(&SYNAPSE, "input_tokens", CachedUsageCheck { allowed: true });

        cache.invalidate_entitlement(Some("synapse"), "user", "usr_1", "api_access");
        cache.invalidate_usage(Some("synapse"), "user", "usr_1", "input_tokens");

        assert!(cache.get_entitlement(&SYNAPSE, "api_access").is_none());
        assert!(cache.get_usage(&SYNAPSE, "input_tokens").is_none());
    }

    #[test]
    fn scoped_invalidation_leaves_other_products() {
        let cache = EntitlementCache::new(60);
        cache.put_entitlement(&SYNAPSE, "api_access", granted());
        cache.put_entitlement(&FACET, "api_access", granted());
        cache.put_token_limits(&SYNAPSE, limits(8192));
        cache.put_token_limits(&FACET, limits(8192));

        cache.invalidate_entitlement(Some("facet"), "user", "usr_1", "api_access");
        cache.invalidate_token_limits(Some("facet"), "user", "usr_1");

        assert!(cache.get_entitlement(&FACET, "api_access").is_none());
        assert!(cache.get_token_limits(&FACET).is_none());
        assert!(cache.get_entitlement(&SYNAPSE, "api_access").is_some());
        assert!(cache.get_token_limits(&SYNAPSE).is_some());
    }

    #[test]
    fn unscoped_invalidation_clears_entity_in_every_product() {
        // A sender that does not name the product must still never leave a
        // stale grant behind, so it clears the entity across all products
        let cache = EntitlementCache::new(60);
        cache.put_entitlement(&SYNAPSE, "api_access", granted());
        cache.put_entitlement(&FACET, "api_access", granted());
        cache.put_usage(&FACET, "requests", CachedUsageCheck { allowed: true });
        cache.put_token_limits(&SYNAPSE, limits(8192));
        cache.put_token_limits(&FACET, limits(8192));

        cache.invalidate_entitlement(None, "user", "usr_1", "api_access");
        cache.invalidate_usage(None, "user", "usr_1", "requests");
        cache.invalidate_token_limits(None, "user", "usr_1");

        assert!(cache.get_entitlement(&SYNAPSE, "api_access").is_none());
        assert!(cache.get_entitlement(&FACET, "api_access").is_none());
        assert!(cache.get_usage(&FACET, "requests").is_none());
        assert!(cache.get_token_limits(&SYNAPSE).is_none());
        assert!(cache.get_token_limits(&FACET).is_none());
    }
}
