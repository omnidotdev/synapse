//! Whether an entity may spend the pooled credit wallet on managed AI
//!
//! A free Omni account costs nothing to create, and the pooled wallet holds
//! the free credit grants of every product the account touched, so a wallet
//! funded only by free grants can be farmed across fresh accounts. Managed AI
//! billed to the wallet is therefore served only to a paying customer: an
//! entity on a paid tier of the product, or one that has bought credits

use std::time::Duration;

use mini_moka::sync::Cache;

/// Entitlement feature key holding an entity's tier in a product
pub const TIER_FEATURE_KEY: &str = "tier";

/// Tier name of the free plan in every product
const FREE_TIER: &str = "free";

/// How long a paying customer is remembered
///
/// Safe to keep for minutes: the wallet balance check still runs on every
/// request, so a customer who refunds or downgrades meanwhile can at most
/// spend credits that are already in the wallet
const PAYING_TTL: Duration = Duration::from_secs(300);

/// How long a non-paying entity is remembered
///
/// Short, so a purchase or an upgrade unlocks managed AI within seconds,
/// while a burst of refused requests does not reach Aether on every call
const NOT_PAYING_TTL: Duration = Duration::from_secs(15);

/// The product and entity an eligibility decision was made for
///
/// The product is part of the key: a tier is per product, so a decision for
/// one product must never answer for another
type GateKey = (String, String, String);

/// Shared cache of paying-customer decisions
///
/// Cheap to clone; every clone shares one cache, so the clients of every
/// modality reuse each other's answers. Only an answer Aether actually gave
/// is cached, never a failure to reach it
#[derive(Clone)]
pub struct PayingCustomerGate {
    paying: Cache<GateKey, ()>,
    not_paying: Cache<GateKey, ()>,
}

impl PayingCustomerGate {
    /// Create a gate with an empty cache
    #[must_use]
    pub fn new() -> Self {
        Self::with_ttls(PAYING_TTL, NOT_PAYING_TTL)
    }

    fn with_ttls(paying_ttl: Duration, not_paying_ttl: Duration) -> Self {
        Self {
            paying: Cache::builder().max_capacity(10_000).time_to_live(paying_ttl).build(),
            not_paying: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(not_paying_ttl)
                .build(),
        }
    }

    /// The cached decision for an entity in a product, if any
    pub(crate) fn cached(&self, app_id: &str, entity_type: &str, entity_id: &str) -> Option<bool> {
        let key = key(app_id, entity_type, entity_id);
        if self.paying.contains_key(&key) {
            Some(true)
        } else if self.not_paying.contains_key(&key) {
            Some(false)
        } else {
            None
        }
    }

    /// Remember a decision Aether gave for an entity in a product
    pub(crate) fn remember(&self, app_id: &str, entity_type: &str, entity_id: &str, is_paying: bool) {
        let key = key(app_id, entity_type, entity_id);
        if is_paying {
            self.not_paying.invalidate(&key);
            self.paying.insert(key, ());
        } else {
            self.paying.invalidate(&key);
            self.not_paying.insert(key, ());
        }
    }
}

impl Default for PayingCustomerGate {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PayingCustomerGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PayingCustomerGate").finish_non_exhaustive()
    }
}

fn key(app_id: &str, entity_type: &str, entity_id: &str) -> GateKey {
    (app_id.to_owned(), entity_type.to_owned(), entity_id.to_owned())
}

/// Whether a `tier` entitlement value names a paid tier
///
/// Anything but the free tier is paid (an Omni platform org resolves to
/// `team`). An absent, empty, or non-string value is treated as free, so an
/// unexpected value can only send the decision on to the funding check
#[must_use]
pub fn is_paid_tier(value: Option<&serde_json::Value>) -> bool {
    value
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .is_some_and(|tier| !tier.is_empty() && !tier.eq_ignore_ascii_case(FREE_TIER))
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    #[test]
    fn paid_tiers_are_anything_but_free() {
        assert!(is_paid_tier(Some(&json!("pro"))));
        assert!(is_paid_tier(Some(&json!("team"))));
        assert!(is_paid_tier(Some(&json!("enterprise"))));
    }

    #[test]
    fn free_absent_and_malformed_tiers_are_not_paid() {
        assert!(!is_paid_tier(Some(&json!("free"))));
        assert!(!is_paid_tier(Some(&json!(" Free "))));
        assert!(!is_paid_tier(Some(&json!(""))));
        assert!(!is_paid_tier(Some(&json!(null))));
        assert!(!is_paid_tier(Some(&json!(1))));
        assert!(!is_paid_tier(Some(&json!(true))));
        assert!(!is_paid_tier(None));
    }

    #[test]
    fn decisions_are_scoped_to_product_and_entity() {
        let gate = PayingCustomerGate::new();

        gate.remember("facet", "user", "usr_1", true);

        assert_eq!(gate.cached("facet", "user", "usr_1"), Some(true));
        assert_eq!(gate.cached("keystone", "user", "usr_1"), None);
        assert_eq!(gate.cached("facet", "organization", "usr_1"), None);
        assert_eq!(gate.cached("facet", "user", "usr_2"), None);
    }

    #[test]
    fn a_new_decision_replaces_the_old_one() {
        let gate = PayingCustomerGate::new();

        gate.remember("facet", "user", "usr_1", false);
        assert_eq!(gate.cached("facet", "user", "usr_1"), Some(false));

        gate.remember("facet", "user", "usr_1", true);
        assert_eq!(gate.cached("facet", "user", "usr_1"), Some(true));

        gate.remember("facet", "user", "usr_1", false);
        assert_eq!(gate.cached("facet", "user", "usr_1"), Some(false));
    }

    #[test]
    fn a_not_paying_decision_expires_on_its_short_ttl() {
        let gate = PayingCustomerGate::with_ttls(Duration::from_secs(60), Duration::from_millis(20));

        gate.remember("facet", "user", "usr_1", false);
        gate.remember("facet", "user", "usr_2", true);
        std::thread::sleep(Duration::from_millis(60));

        assert_eq!(gate.cached("facet", "user", "usr_1"), None);
        assert_eq!(gate.cached("facet", "user", "usr_2"), Some(true));
    }
}
