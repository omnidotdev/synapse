//! Markup applied to upstream cost for managed (Omni-key) usage

use std::collections::HashMap;

/// Retail markup for product-scoped usage when no margin is configured
pub const DEFAULT_PRODUCT_MARGIN: f64 = 2.0;

/// How managed usage is marked up, per product and per plan
///
/// Traffic billed to this gateway's own product (`default_app_id`, or a
/// request with no product) uses the tier margin for its plan, then the
/// provider margin. Traffic billed to another product uses that product's
/// margin, never this gateway's tier margins: those are keyed by this
/// product's plan names, which mean something else in another product
#[derive(Debug, Clone)]
pub struct MarginPolicy {
    /// App id of the gateway's own product
    pub default_app_id: String,
    /// Provider name to margin, for the gateway's own product
    pub provider_margins: HashMap<String, f64>,
    /// Plan name to margin, for the gateway's own product
    pub tier_margins: HashMap<String, f64>,
    /// App id to margin, for product-scoped traffic
    pub product_margins: HashMap<String, f64>,
    /// Margin for product-scoped traffic with no entry in `product_margins`
    pub default_product_margin: f64,
}

impl Default for MarginPolicy {
    fn default() -> Self {
        Self {
            default_app_id: String::new(),
            provider_margins: HashMap::new(),
            tier_margins: HashMap::new(),
            product_margins: HashMap::new(),
            default_product_margin: DEFAULT_PRODUCT_MARGIN,
        }
    }
}

impl MarginPolicy {
    /// Amount to charge for managed usage costing `base_usd` upstream
    ///
    /// Bring-your-own-key usage costs the gateway nothing and is never charged
    #[must_use]
    pub fn charge(&self, identity: &crate::BillingIdentity, provider_name: &str, base_usd: f64) -> f64 {
        if identity.mode != crate::BillingMode::Managed {
            return 0.0;
        }
        base_usd * self.resolve(Some(&identity.app_id), identity.plan.as_deref(), provider_name)
    }

    /// Resolve the margin for a request billed to `app_id` on `plan`
    #[must_use]
    pub fn resolve(&self, app_id: Option<&str>, plan: Option<&str>, provider_name: &str) -> f64 {
        match app_id {
            Some(app) if app != self.default_app_id => self
                .product_margins
                .get(app)
                .copied()
                .unwrap_or(self.default_product_margin),
            _ => plan
                .and_then(|p| self.tier_margins.get(p).copied())
                .or_else(|| self.provider_margins.get(provider_name).copied())
                .unwrap_or(1.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn margin_policy() -> MarginPolicy {
        MarginPolicy {
            default_app_id: "synapse".to_owned(),
            provider_margins: HashMap::from([("openai".to_owned(), 1.3)]),
            tier_margins: HashMap::from([("free".to_owned(), 1.2), ("pro".to_owned(), 1.15)]),
            product_margins: HashMap::from([("facet".to_owned(), 2.5)]),
            default_product_margin: 2.0,
        }
    }

    fn assert_margin(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "margin {actual}, expected {expected}"
        );
    }

    #[test]
    fn own_product_tier_margin_takes_precedence() {
        let policy = margin_policy();

        // Tier margin wins when plan matches, whether the app is explicit or not
        assert_margin(policy.resolve(Some("synapse"), Some("free"), "openai"), 1.2);
        assert_margin(policy.resolve(None, Some("free"), "openai"), 1.2);

        // Falls back to provider margin when plan has no tier margin
        assert_margin(policy.resolve(None, Some("enterprise"), "openai"), 1.3);

        // Falls back to provider margin when no plan
        assert_margin(policy.resolve(None, None, "openai"), 1.3);

        // Falls back to 1.0 when nothing matches
        assert_margin(policy.resolve(None, None, "unknown"), 1.0);
    }

    #[test]
    fn product_scoped_request_uses_its_product_margin_not_the_gateway_tier() {
        // A facet "pro" user must not get this gateway's 1.15 "pro" margin
        assert_margin(margin_policy().resolve(Some("facet"), Some("pro"), "openai"), 2.5);
    }

    fn identity(app_id: &str, plan: Option<&str>, mode: crate::BillingMode) -> crate::BillingIdentity {
        crate::BillingIdentity {
            app_id: app_id.to_owned(),
            entity_type: "user".to_owned(),
            entity_id: "u1".to_owned(),
            mode,
            plan: plan.map(str::to_owned),
        }
    }

    #[test]
    fn charge_marks_up_managed_usage_for_the_billed_product() {
        let policy = margin_policy();
        let facet = identity("facet", Some("pro"), crate::BillingMode::Managed);
        let own = identity("synapse", Some("free"), crate::BillingMode::Managed);
        assert_margin(policy.charge(&facet, "openai", 1.0), 2.5);
        assert_margin(policy.charge(&own, "openai", 1.0), 1.2);
    }

    #[test]
    fn charge_is_zero_for_bring_your_own_key() {
        let byok = identity("facet", None, crate::BillingMode::Byok);
        assert_margin(margin_policy().charge(&byok, "openai", 1.0), 0.0);
    }

    #[test]
    fn unconfigured_product_uses_the_retail_default() {
        let policy = margin_policy();
        assert_margin(policy.resolve(Some("halo"), Some("free"), "openai"), 2.0);
        assert_margin(policy.resolve(Some("halo"), None, "unknown"), 2.0);
    }
}
