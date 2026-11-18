//! Per-product chat model policy for product-scoped keys
//!
//! A product with a policy may only be served its listed models, lists
//! exactly those from `GET /v1/models`, and has the virtual `auto` model
//! resolve to its default. Keys of the gateway's own product, and products
//! without a policy, are not restricted

use std::collections::HashMap;

use synapse_config::ProductModelPolicyConfig;
use synapse_core::RequestContext;

/// The chat models one product may use
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductModelPolicy {
    /// Allowed (provider, model) pairs, in listing order
    models: Vec<(String, String)>,
    /// Index into `models` of the default
    default_index: usize,
    /// Index into `models` of the model the card-verified free allowance is
    /// served, if the product serves it at all
    free_index: Option<usize>,
}

impl ProductModelPolicy {
    /// Build a policy from validated config
    ///
    /// Returns `None` for a policy that fails validation, which config
    /// loading rejects before this is reached
    pub fn from_config(config: &ProductModelPolicyConfig) -> Option<Self> {
        let models: Vec<(String, String)> = config
            .models
            .iter()
            .map(|id| id.split_once('/').map(|(p, m)| (p.to_owned(), m.to_owned())))
            .collect::<Option<_>>()?;
        let default_index = config.models.iter().position(|id| id == &config.default_model)?;
        let free_index = match &config.free_model {
            Some(free) => Some(config.models.iter().position(|id| id == free)?),
            None => None,
        };
        Some(Self {
            models,
            default_index,
            free_index,
        })
    }

    /// Allowed (provider, model) pairs, in listing order
    pub fn models(&self) -> &[(String, String)] {
        &self.models
    }

    /// The (provider, model) pair `auto` resolves to
    pub fn default_model(&self) -> (&str, &str) {
        let (provider, model) = &self.models[self.default_index];
        (provider, model)
    }

    /// The (provider, model) pair the card-verified free allowance is held
    /// to, if the product serves the free allowance
    pub fn free_model(&self) -> Option<(&str, &str)> {
        let (provider, model) = &self.models[self.free_index?];
        Some((provider, model))
    }

    /// Whether `provider`/`model` is the default
    pub fn is_default(&self, provider: &str, model: &str) -> bool {
        self.default_model() == (provider, model)
    }

    /// Whether the product may be served `provider`/`model`
    pub fn allows(&self, provider: &str, model: &str) -> bool {
        self.models.iter().any(|(p, m)| p == provider && m == model)
    }
}

/// Model policies for every product that has one
#[derive(Debug, Clone, Default)]
pub struct ProductPolicies {
    /// App id of the gateway's own product, never restricted
    default_app_id: String,
    policies: HashMap<String, ProductModelPolicy>,
}

impl ProductPolicies {
    /// Build the policies from config, keyed by product app id
    pub fn new<'a>(
        default_app_id: &str,
        products: impl IntoIterator<Item = (&'a String, &'a ProductModelPolicyConfig)>,
    ) -> Self {
        let policies = products
            .into_iter()
            .filter(|(product, _)| product.as_str() != default_app_id)
            .filter_map(|(product, config)| Some((product.clone(), ProductModelPolicy::from_config(config)?)))
            .collect();
        Self {
            default_app_id: default_app_id.to_owned(),
            policies,
        }
    }

    /// Every product's policy, keyed by app id
    pub fn iter(&self) -> impl Iterator<Item = (&String, &ProductModelPolicy)> {
        self.policies.iter()
    }

    /// The policy for the product a request bills against, if it has one
    pub fn for_context(&self, context: &RequestContext) -> Option<&ProductModelPolicy> {
        let app_id = &context.billing_identity.as_ref()?.app_id;
        if *app_id == self.default_app_id {
            return None;
        }
        self.policies.get(app_id)
    }
}

/// A human-readable model name derived from its id, for models configured
/// without a display name (e.g. "claude-sonnet-5-5" is "Claude Sonnet 5.5")
pub fn derived_display_name(model: &str) -> String {
    // A namespaced id ("moonshotai/kimi-k2.5") is named by its last segment
    let name = model.rsplit('/').next().unwrap_or(model);
    let mut words: Vec<String> = Vec::new();
    let mut version: Vec<&str> = Vec::new();
    let flush = |words: &mut Vec<String>, version: &mut Vec<&str>| {
        if !version.is_empty() {
            words.push(version.join("."));
            version.clear();
        }
    };
    for part in name.split('-') {
        let numeric = !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        // A trailing date snapshot (e.g. 20250929) is not part of the name
        if numeric && part.len() == 8 {
            continue;
        }
        if numeric {
            version.push(part);
            continue;
        }
        flush(&mut words, &mut version);
        let word = if part == "gpt" {
            "GPT".to_owned()
        } else {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        };
        words.push(word);
    }
    flush(&mut words, &mut version);
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(models: &[&str], default_model: &str) -> ProductModelPolicyConfig {
        ProductModelPolicyConfig {
            models: models.iter().map(|m| (*m).to_owned()).collect(),
            default_model: default_model.to_owned(),
            free_model: None,
        }
    }

    fn context(app_id: &str) -> RequestContext {
        let mut ctx = RequestContext::empty();
        ctx.billing_identity = Some(synapse_core::BillingIdentity {
            app_id: app_id.to_owned(),
            entity_type: "user".to_owned(),
            entity_id: "u1".to_owned(),
            mode: synapse_core::BillingMode::Managed,
            plan: None,
        });
        ctx
    }

    fn policies() -> ProductPolicies {
        let products = HashMap::from([
            (
                "keystone".to_owned(),
                config(
                    &["anthropic/claude-sonnet-5-5", "anthropic/claude-haiku-5-5"],
                    "anthropic/claude-sonnet-5-5",
                ),
            ),
            ("synapse".to_owned(), config(&["anthropic/x"], "anthropic/x")),
        ]);
        ProductPolicies::new("synapse", &products)
    }

    #[test]
    fn a_product_key_gets_its_policy() {
        let policies = policies();
        let policy = policies.for_context(&context("keystone")).unwrap();
        assert!(policy.allows("anthropic", "claude-haiku-5-5"));
        assert!(!policy.allows("openai", "gpt-4o"));
        assert!(!policy.allows("anthropic", "claude-opus-5-5"));
        assert_eq!(policy.default_model(), ("anthropic", "claude-sonnet-5-5"));
        assert!(policy.is_default("anthropic", "claude-sonnet-5-5"));
        assert!(!policy.is_default("anthropic", "claude-haiku-5-5"));
    }

    #[test]
    fn the_gateway_own_keys_and_unlisted_products_are_unrestricted() {
        let policies = policies();
        assert!(policies.for_context(&context("synapse")).is_none());
        assert!(policies.for_context(&context("facet")).is_none());
        assert!(policies.for_context(&RequestContext::empty()).is_none());
    }

    #[test]
    fn a_policy_keeps_its_listing_order() {
        let policy = ProductModelPolicy::from_config(&config(
            &["nvidia/moonshotai/kimi-k2.5", "anthropic/claude-haiku-5-5"],
            "anthropic/claude-haiku-5-5",
        ))
        .unwrap();
        assert_eq!(
            policy.models(),
            [
                ("nvidia".to_owned(), "moonshotai/kimi-k2.5".to_owned()),
                ("anthropic".to_owned(), "claude-haiku-5-5".to_owned()),
            ]
        );
    }

    #[test]
    fn display_names_derive_from_model_ids() {
        assert_eq!(derived_display_name("claude-sonnet-5-5"), "Claude Sonnet 5.5");
        assert_eq!(derived_display_name("claude-haiku-4-5-20251001"), "Claude Haiku 4.5");
        assert_eq!(derived_display_name("claude-opus-5"), "Claude Opus 5");
        assert_eq!(derived_display_name("gpt-4o-mini"), "GPT 4o Mini");
        assert_eq!(derived_display_name("moonshotai/kimi-k2.5"), "Kimi K2.5");
    }
}
