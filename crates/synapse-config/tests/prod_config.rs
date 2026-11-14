//! Guard the production config: it must parse and pass boot validation
//! (priced managed models, billing rules), so a bad edit fails CI rather than
//! the production rollout

#[test]
fn prod_config_parses_and_validates() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/synapse.prod.toml");
    let raw = std::fs::read_to_string(path).expect("read prod config");

    // Every `{{ env.* }}` placeholder sits inside a string; a URL-shaped value
    // satisfies both URL and secret fields
    let placeholders = regex::Regex::new(r"\{\{.*?\}\}").expect("placeholder pattern");
    let expanded = placeholders.replace_all(&raw, "http://placeholder.invalid");

    let config: synapse_config::Config = toml::from_str(&expanded).expect("parse prod config");
    config.validate().expect("prod config passes boot validation");
}

/// Posture C products (AI included in the SaaS tier) must be declared with
/// their allowance meter and the fleet retail margin, or their AI would draw
/// on the pooled wallet instead of the included allowance
#[test]
fn prod_config_declares_posture_c_products() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/synapse.prod.toml");
    let raw = std::fs::read_to_string(path).expect("read prod config");
    let placeholders = regex::Regex::new(r"\{\{.*?\}\}").expect("placeholder pattern");
    let expanded = placeholders.replace_all(&raw, "http://placeholder.invalid");
    let config: synapse_config::Config = toml::from_str(&expanded).expect("parse prod config");
    let billing = config.billing.expect("billing configured");

    for product in ["myfi", "backfeed"] {
        assert_eq!(
            billing.included_allowance_products.get(product).map(String::as_str),
            Some("ai_credits"),
            "{product} must bill its included allowance meter"
        );
        assert!(
            (billing.product_margins.get(product).copied().unwrap_or_default() - 2.0).abs() < f64::EPSILON,
            "{product} must carry the fleet retail margin"
        );
    }
}

/// Every product with a product-scoped chat key has a model policy, so its
/// picker and default come from the gateway, and the policy matches what the
/// product sends today (each listed model is priced: `validate` checks that)
#[test]
fn prod_config_declares_product_model_policies() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/synapse.prod.toml");
    let raw = std::fs::read_to_string(path).expect("read prod config");
    let placeholders = regex::Regex::new(r"\{\{.*?\}\}").expect("placeholder pattern");
    let expanded = placeholders.replace_all(&raw, "http://placeholder.invalid");
    let config: synapse_config::Config = toml::from_str(&expanded).expect("parse prod config");
    let billing = config.billing.expect("billing configured");

    let keystone = &billing.products["keystone"];
    assert_eq!(
        keystone.models,
        [
            "anthropic/claude-sonnet-5-5",
            "anthropic/claude-opus-5-5",
            "anthropic/claude-haiku-5-5"
        ]
    );
    assert_eq!(keystone.default_model, "anthropic/claude-sonnet-5-5");
    // Beacon's free default is Haiku-class
    assert_eq!(billing.products["beacon"].default_model, "anthropic/claude-haiku-5-5");
    for product in ["facet", "creator", "myfi"] {
        assert!(billing.products.contains_key(product), "{product} has a model policy");
    }

    // Smart routing's threshold models are current, priced models
    let threshold = &config.llm.routing.threshold;
    for id in [&threshold.low_complexity_model, &threshold.high_complexity_model] {
        let (provider, model) = id
            .as_deref()
            .and_then(|id| id.split_once('/'))
            .expect("threshold model");
        assert!(
            config
                .llm
                .routing
                .models
                .iter()
                .any(|p| p.provider == provider && p.model == model),
            "{provider}/{model} is priced"
        );
    }
}
