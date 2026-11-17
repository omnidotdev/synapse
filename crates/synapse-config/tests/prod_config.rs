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

/// Posture C products (AI included in the `SaaS` tier) must be declared with
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

/// A wallet-billed managed request must never be served unbilled during an
/// Aether outage, whatever the general fail mode is
#[test]
fn prod_config_fails_wallet_spend_checks_closed() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/synapse.prod.toml");
    let raw = std::fs::read_to_string(path).expect("read prod config");
    let placeholders = regex::Regex::new(r"\{\{.*?\}\}").expect("placeholder pattern");
    let expanded = placeholders.replace_all(&raw, "http://placeholder.invalid");
    let config: synapse_config::Config = toml::from_str(&expanded).expect("parse prod config");
    let billing = config.billing.expect("billing configured");

    assert_eq!(billing.wallet_fail_mode, synapse_config::FailMode::Closed);
    assert_eq!(
        billing.spend_fail_modes().for_spend(true),
        &synapse_config::FailMode::Closed
    );
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
    // Beacon's free default is Haiku-class; its picker offers the current
    // Claude line only, cheapest first
    let beacon = &billing.products["beacon"];
    assert_eq!(beacon.default_model, "anthropic/claude-haiku-5-5");
    assert_eq!(
        beacon.models,
        [
            "anthropic/claude-haiku-5-5",
            "anthropic/claude-sonnet-5-5",
            "anthropic/claude-opus-5-5"
        ]
    );

    // Facet runs Sonnet 5.5 (its configured model and fallback); Opus 5.5 is
    // the documented max-quality override
    let facet = &billing.products["facet"];
    assert_eq!(facet.default_model, "anthropic/claude-sonnet-5-5");
    assert_eq!(
        facet.models,
        ["anthropic/claude-sonnet-5-5", "anthropic/claude-opus-5-5"]
    );

    // myfi sends `auto` for categorization, on Haiku 5.5 (an included allowance)
    let myfi = &billing.products["myfi"];
    assert_eq!(myfi.default_model, "anthropic/claude-haiku-5-5");
    assert_eq!(myfi.models, ["anthropic/claude-haiku-5-5"]);

    // Creator reads its picker from the gateway; Haiku 5.5 default, with
    // GPT-4o mini kept as its OpenAI option
    let creator = &billing.products["creator"];
    assert_eq!(creator.default_model, "anthropic/claude-haiku-5-5");
    assert_eq!(
        creator.models,
        [
            "anthropic/claude-haiku-5-5",
            "anthropic/claude-sonnet-5-5",
            "openai/gpt-4o-mini"
        ]
    );

    // No product is offered a retired-generation or superseded model
    let retired = [
        "anthropic/claude-sonnet-4-5-20250929",
        "anthropic/claude-sonnet-4-6",
        "anthropic/claude-haiku-4-5-20251001",
        "anthropic/claude-opus-4-6",
        "openai/gpt-4o",
    ];
    for (product, policy) in &billing.products {
        for model in &policy.models {
            assert!(!retired.contains(&model.as_str()), "{product} still offers {model}");
        }
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

/// Model ids Anthropic has retired (requests fail upstream); none may appear
/// anywhere in the config
const RETIRED_MODELS: &[&str] = &[
    "anthropic/claude-sonnet-4-20250514",
    "anthropic/claude-opus-4-20250514",
    "anthropic/claude-opus-4-1-20250805",
    "anthropic/claude-3-7-sonnet-20250219",
    "anthropic/claude-3-5-sonnet-20241022",
    "anthropic/claude-3-5-haiku-20241022",
    "anthropic/claude-3-haiku-20240307",
    // NVIDIA's catalog dropped it for moonshotai/kimi-k2.6 (404 upstream)
    "nvidia/moonshotai/kimi-k2.5",
];

/// Deprecated upstream and retiring soon: may stay priced for direct keys
/// until retirement, but never be a failover, routing, or product target
const DEPRECATED_MODELS: &[&str] = &["anthropic/claude-sonnet-4-5-20250929"];

/// Failover and routing only ever land on current, priced models, and every
/// failover group's first fallback crosses vendors
#[test]
fn prod_config_failover_and_routing_use_current_priced_models() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/synapse.prod.toml");
    let raw = std::fs::read_to_string(path).expect("read prod config");
    let placeholders = regex::Regex::new(r"\{\{.*?\}\}").expect("placeholder pattern");
    let expanded = placeholders.replace_all(&raw, "http://placeholder.invalid");
    let config: synapse_config::Config = toml::from_str(&expanded).expect("parse prod config");

    let priced: Vec<String> = config
        .llm
        .routing
        .models
        .iter()
        .map(|m| format!("{}/{}", m.provider, m.model))
        .collect();

    // No retired id is priced (a priced retired model 404s upstream after
    // passing the spend check)
    for id in &priced {
        assert!(!RETIRED_MODELS.contains(&id.as_str()), "{id} is retired but priced");
    }

    let current = |id: &str, role: &str| {
        assert!(priced.iter().any(|p| p == id), "{role} {id} is not priced");
        assert!(!RETIRED_MODELS.contains(&id), "{role} {id} is retired");
        assert!(!DEPRECATED_MODELS.contains(&id), "{role} {id} is deprecated");
    };

    let threshold = &config.llm.routing.threshold;
    for id in [&threshold.low_complexity_model, &threshold.high_complexity_model]
        .into_iter()
        .flatten()
    {
        current(id, "threshold model");
    }

    let groups = &config.llm.failover.equivalence_groups;
    assert!(!groups.is_empty(), "failover groups configured");
    for group in groups {
        for id in &group.models {
            current(id, &format!("failover group {}", group.name));
        }
        let providers: std::collections::HashSet<_> = group
            .models
            .iter()
            .filter_map(|m| m.split_once('/').map(|(p, _)| p))
            .collect();
        assert!(providers.len() >= 2, "failover group {} spans vendors", group.name);
    }

    // The gateway tries the first other member of the first group listing a
    // model; that fallback must be another vendor, or a provider outage fails
    // over into the same outage
    for id in groups.iter().flat_map(|g| g.models.iter()) {
        let group = groups.iter().find(|g| g.models.contains(id)).expect("group");
        let fallback = group.models.iter().find(|m| *m != id).expect("fallback");
        let vendor = |m: &str| m.split_once('/').map(|(p, _)| p.to_owned());
        assert_ne!(
            vendor(id),
            vendor(fallback),
            "{id} falls back to {fallback}, the same vendor"
        );
    }

    if let Some(billing) = &config.billing {
        for (product, policy) in &billing.products {
            for id in &policy.models {
                current(id, &format!("{product} policy model"));
            }
        }
    }
}
