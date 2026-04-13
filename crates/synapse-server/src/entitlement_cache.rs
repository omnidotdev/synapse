use std::time::Duration;

use mini_moka::sync::Cache;
use synapse_core::TokenLimits;

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

/// In-memory TTL cache for entitlement and usage checks
///
/// Reduces load on Aether by caching positive results for a
/// configurable duration
#[derive(Clone)]
pub struct EntitlementCache {
    entitlements: Cache<String, CachedEntitlement>,
    usage: Cache<String, CachedUsageCheck>,
    token_limits: Cache<String, CachedTokenLimits>,
}

impl EntitlementCache {
    /// Create a new cache with the given TTL in seconds
    pub fn new(ttl_secs: u64) -> Self {
        let ttl = Duration::from_secs(ttl_secs);

        Self {
            entitlements: Cache::builder().max_capacity(10_000).time_to_live(ttl).build(),
            usage: Cache::builder().max_capacity(10_000).time_to_live(ttl).build(),
            token_limits: Cache::builder().max_capacity(10_000).time_to_live(ttl).build(),
        }
    }

    /// Look up a cached entitlement check
    pub fn get_entitlement(&self, entity_type: &str, entity_id: &str, feature_key: &str) -> Option<CachedEntitlement> {
        let key = format!("{entity_type}:{entity_id}:{feature_key}");
        self.entitlements.get(&key)
    }

    /// Cache an entitlement check result
    pub fn put_entitlement(&self, entity_type: &str, entity_id: &str, feature_key: &str, result: CachedEntitlement) {
        let key = format!("{entity_type}:{entity_id}:{feature_key}");
        self.entitlements.insert(key, result);
    }

    /// Look up a cached usage check
    pub fn get_usage(&self, entity_type: &str, entity_id: &str, meter_key: &str) -> Option<CachedUsageCheck> {
        let key = format!("{entity_type}:{entity_id}:{meter_key}");
        self.usage.get(&key)
    }

    /// Cache a usage check result
    pub fn put_usage(&self, entity_type: &str, entity_id: &str, meter_key: &str, result: CachedUsageCheck) {
        let key = format!("{entity_type}:{entity_id}:{meter_key}");
        self.usage.insert(key, result);
    }

    /// Look up cached per-request token limits
    pub fn get_token_limits(&self, entity_type: &str, entity_id: &str) -> Option<CachedTokenLimits> {
        let key = format!("{entity_type}:{entity_id}:token_limits");
        self.token_limits.get(&key)
    }

    /// Cache per-request token limits
    pub fn put_token_limits(&self, entity_type: &str, entity_id: &str, result: CachedTokenLimits) {
        let key = format!("{entity_type}:{entity_id}:token_limits");
        self.token_limits.insert(key, result);
    }

    /// Invalidate cached per-request token limits (used by entitlement webhooks)
    pub fn invalidate_token_limits(&self, entity_type: &str, entity_id: &str) {
        let key = format!("{entity_type}:{entity_id}:token_limits");
        self.token_limits.invalidate(&key);
    }

    /// Invalidate a specific cached entitlement
    pub fn invalidate_entitlement(&self, entity_type: &str, entity_id: &str, feature_key: &str) {
        let key = format!("{entity_type}:{entity_id}:{feature_key}");
        self.entitlements.invalidate(&key);
    }

    /// Invalidate a specific cached usage check
    pub fn invalidate_usage(&self, entity_type: &str, entity_id: &str, meter_key: &str) {
        let key = format!("{entity_type}:{entity_id}:{meter_key}");
        self.usage.invalidate(&key);
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

    #[test]
    fn cache_miss_returns_none() {
        let cache = EntitlementCache::new(60);
        assert!(cache.get_entitlement("user", "usr_1", "api_access").is_none());
        assert!(cache.get_usage("user", "usr_1", "input_tokens").is_none());
    }

    #[test]
    fn cache_hit_returns_value() {
        let cache = EntitlementCache::new(60);

        cache.put_entitlement(
            "user",
            "usr_1",
            "api_access",
            CachedEntitlement {
                has_access: true,
                version: Some(1),
            },
        );

        let result = cache.get_entitlement("user", "usr_1", "api_access");
        assert!(result.is_some());
        assert!(result.unwrap().has_access);
    }

    #[test]
    fn cache_usage_hit() {
        let cache = EntitlementCache::new(60);

        cache.put_usage("user", "usr_1", "input_tokens", CachedUsageCheck { allowed: true });

        let result = cache.get_usage("user", "usr_1", "input_tokens");
        assert!(result.is_some());
        assert!(result.unwrap().allowed);
    }

    #[test]
    fn cache_token_limits_hit() {
        let cache = EntitlementCache::new(60);

        cache.put_token_limits(
            "user",
            "usr_1",
            CachedTokenLimits {
                limits: TokenLimits {
                    max_input_tokens: 8192,
                    max_output_tokens: 4096,
                },
            },
        );

        let result = cache.get_token_limits("user", "usr_1");
        assert!(result.is_some());
        let limits = result.unwrap().limits;
        assert_eq!(limits.max_input_tokens, 8192);
        assert_eq!(limits.max_output_tokens, 4096);
    }

    #[test]
    fn cache_token_limits_miss() {
        let cache = EntitlementCache::new(60);
        assert!(cache.get_token_limits("user", "usr_1").is_none());
    }

    #[test]
    fn invalidate_clears_specific_entries() {
        let cache = EntitlementCache::new(60);

        cache.put_entitlement(
            "user",
            "usr_1",
            "api_access",
            CachedEntitlement {
                has_access: true,
                version: Some(1),
            },
        );
        cache.put_usage("user", "usr_1", "input_tokens", CachedUsageCheck { allowed: true });

        cache.invalidate_entitlement("user", "usr_1", "api_access");
        cache.invalidate_usage("user", "usr_1", "input_tokens");

        assert!(cache.get_entitlement("user", "usr_1", "api_access").is_none());
        assert!(cache.get_usage("user", "usr_1", "input_tokens").is_none());
    }
}
