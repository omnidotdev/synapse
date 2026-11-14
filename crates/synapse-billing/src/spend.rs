//! Where a product's managed AI spend is charged
//!
//! Most products bill managed AI against the billing account's pooled credit
//! wallet (prepaid credits). A product whose AI is included in its plan tier
//! (Posture C) instead draws on a monthly fair-use allowance tracked as an
//! Aether usage meter in credits, so its AI can never spend the wallet

use std::collections::HashMap;

/// Credits per retail USD (1 credit is $0.01)
pub const CREDITS_PER_USD: f64 = 100.0;

/// Convert a retail USD amount to credits
#[must_use]
pub fn usd_to_credits(usd: f64) -> f64 {
    usd * CREDITS_PER_USD
}

/// Per-product choice of where managed AI spend is charged
#[derive(Debug, Clone, Default)]
pub struct SpendPolicy {
    /// App id to the usage meter holding that product's included allowance
    allowance_meters: HashMap<String, String>,
}

impl SpendPolicy {
    /// Build a policy from app id to allowance meter key; every other product
    /// bills the credit wallet
    #[must_use]
    pub const fn new(allowance_meters: HashMap<String, String>) -> Self {
        Self { allowance_meters }
    }

    /// Where spend billed to `app_id` is charged
    #[must_use]
    pub fn target(&self, app_id: &str) -> SpendTarget<'_> {
        self.allowance_meters
            .get(app_id)
            .map_or(SpendTarget::Wallet, |meter_key| SpendTarget::Allowance { meter_key })
    }
}

/// Where one product's managed AI spend is charged
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpendTarget<'a> {
    /// The billing account's pooled credit balance (USD)
    Wallet,
    /// The product's included allowance meter (credits)
    Allowance {
        /// Aether usage meter key
        meter_key: &'a str,
    },
}

/// Outcome of a pre-request spend check
#[derive(Debug, Clone, PartialEq)]
pub enum SpendCheck {
    /// The estimated spend is covered
    Allowed,
    /// The credit wallet cannot cover the estimate
    InsufficientCredits {
        /// Current wallet balance in USD
        balance: f64,
    },
    /// The product's included allowance for this period cannot cover the
    /// estimate
    AllowanceExhausted,
}

/// Actual spend to charge after a request is served
#[derive(Debug, Clone)]
pub struct SpendCharge {
    /// Retail amount in USD (upstream cost with the product's margin)
    pub amount_usd: f64,
    /// What was served (e.g. "anthropic/claude-sonnet-5-5")
    pub description: String,
    /// Unique key so a retried charge is applied once
    pub idempotency_key: String,
    /// Kind of usage (e.g. "completion", "embedding")
    pub reference_type: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlisted_products_bill_the_wallet() {
        let policy = SpendPolicy::new(HashMap::from([("myfi".to_owned(), "ai_credits".to_owned())]));

        assert_eq!(policy.target("facet"), SpendTarget::Wallet);
        assert_eq!(policy.target("synapse"), SpendTarget::Wallet);
        assert_eq!(SpendPolicy::default().target("myfi"), SpendTarget::Wallet);
    }

    #[test]
    fn listed_products_draw_on_their_allowance_meter() {
        let policy = SpendPolicy::new(HashMap::from([("myfi".to_owned(), "ai_credits".to_owned())]));

        assert_eq!(
            policy.target("myfi"),
            SpendTarget::Allowance {
                meter_key: "ai_credits"
            }
        );
    }

    #[test]
    fn credits_are_cents_of_retail_usd() {
        assert!((usd_to_credits(0.25) - 25.0).abs() < 1e-9);
        assert!((usd_to_credits(3.0) - 300.0).abs() < 1e-9);
    }
}
