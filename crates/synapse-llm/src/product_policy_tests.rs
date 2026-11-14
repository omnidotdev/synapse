//! A product-scoped key with a model policy is served only its product's
//! models: `auto` and the other virtual models resolve to its default unless
//! the product is entitled to smart routing (which then routes among its own
//! models), anything else is refused before a provider or the wallet is
//! touched, and failover never leaves the product's models. Billing follows
//! the model that served

use std::collections::HashMap;

use secrecy::SecretString;
use synapse_billing::{AetherClient, MeterKeys, UsageRecorder};
use synapse_config::{
    CircuitBreakerConfig, EquivalenceGroup, FailMode, FailoverConfig, LlmConfig, LlmProviderConfig, LlmProviderType,
    ModelConfig, ModelProfileConfig, ProductModelPolicyConfig, RoutingStrategy,
};
use synapse_core::{HttpError, MarginPolicy};
use url::Url;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::error::LlmError;
use crate::policy::ProductPolicies;
use crate::spend_tests::{BodyApprox, QueryApprox, context, mount_usage_meters, wait_for_request};
use crate::state::{CreditsPerMtok, LlmState};
use crate::types::CompletionRequest;

const CHEAP: &str = "gpt-cheap";
const PRICEY: &str = "gpt-pricey";
const OTHER: &str = "gpt-other";

fn provider_config(server: &MockServer, model: &str) -> LlmProviderConfig {
    LlmProviderConfig {
        provider_type: LlmProviderType::Openai,
        api_key: Some(SecretString::from("provider-key")),
        base_url: Some(format!("{}/v1", server.uri()).parse().unwrap()),
        // Each provider serves only its own model, so a bare model name
        // resolves to one provider deterministically
        models: ModelConfig {
            include: vec![format!("^{model}$")],
            ..ModelConfig::default()
        },
        headers: Vec::new(),
        forward_authorization: false,
        rate_limit: None,
    }
}

fn profile(provider: &str, model: &str, input_per_mtok: f64, output_per_mtok: f64, quality: f64) -> ModelProfileConfig {
    ModelProfileConfig {
        provider: provider.to_owned(),
        model: model.to_owned(),
        display_name: None,
        context_window: 200_000,
        input_per_mtok,
        output_per_mtok,
        quality,
        capabilities: synapse_config::ModelCapabilities::default(),
        long_context_pricing: None,
    }
}

struct Servers {
    cheap: MockServer,
    pricey: MockServer,
    other: MockServer,
    aether: MockServer,
}

impl Servers {
    async fn start() -> Self {
        Self {
            cheap: MockServer::start().await,
            pricey: MockServer::start().await,
            other: MockServer::start().await,
            aether: MockServer::start().await,
        }
    }
}

/// Keystone may use the cheap and pricey models (cheap is its default),
/// never the other one. All three are priced and in one failover group, and
/// the threshold strategy is configured to pick the other model
async fn llm_state(servers: &Servers) -> LlmState {
    let mut config = LlmConfig::default();
    for (name, server, model) in [
        ("cheap", &servers.cheap, CHEAP),
        ("pricey", &servers.pricey, PRICEY),
        ("other", &servers.other, OTHER),
    ] {
        config.providers.insert(name.to_owned(), provider_config(server, model));
    }
    config.failover = FailoverConfig {
        enabled: true,
        max_attempts: 3,
        equivalence_groups: vec![EquivalenceGroup {
            name: "group".to_owned(),
            models: vec![
                format!("cheap/{CHEAP}"),
                format!("other/{OTHER}"),
                format!("pricey/{PRICEY}"),
            ],
        }],
        circuit_breaker: CircuitBreakerConfig {
            error_threshold: 100,
            window_seconds: 60,
            recovery_seconds: 30,
        },
    };
    let mut pricey = profile("pricey", PRICEY, 3000.0, 6000.0, 0.9);
    pricey.display_name = Some("Pricey Model".to_owned());
    config.routing.models = vec![
        profile("cheap", CHEAP, 1000.0, 2000.0, 0.5),
        pricey,
        profile("other", OTHER, 500.0, 1000.0, 0.99),
    ];
    config.routing.enabled = true;
    config.routing.strategy = RoutingStrategy::Threshold;
    config.routing.threshold.low_complexity_model = Some(format!("other/{OTHER}"));
    config.routing.threshold.high_complexity_model = Some(format!("other/{OTHER}"));

    let mut state = LlmState::from_config(config).await.unwrap();
    state.set_margin_policy(MarginPolicy {
        default_app_id: "synapse".to_owned(),
        provider_margins: HashMap::new(),
        tier_margins: HashMap::from([("pro".to_owned(), 1.15)]),
        product_margins: HashMap::from([("keystone".to_owned(), 2.0)]),
        default_product_margin: 2.0,
    });
    let products = HashMap::from([(
        "keystone".to_owned(),
        ProductModelPolicyConfig {
            models: vec![format!("cheap/{CHEAP}"), format!("pricey/{PRICEY}")],
            default_model: format!("cheap/{CHEAP}"),
        },
    )]);
    state.set_product_policies(ProductPolicies::new("synapse", &products));

    let client = AetherClient::new(
        Url::parse(&format!("{}/", servers.aether.uri())).unwrap(),
        SecretString::from("aether-key".to_owned()),
    )
    .unwrap();
    state.set_usage_recorder(UsageRecorder::new(client.clone(), MeterKeys::default()));
    state.set_billing_client(client, FailMode::Closed);
    state
}

fn request(model: &str) -> CompletionRequest {
    serde_json::from_value(serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": "x".repeat(4000) }],
        "params": { "max_tokens": 1000 },
        "stream": false,
    }))
    .unwrap()
}

fn keystone() -> synapse_core::RequestContext {
    context("keystone", "organization", "org_1")
}

async fn mount_completion(server: &MockServer, model: &str, times: u64) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "c1",
            "object": "chat.completion",
            "created": 1,
            "model": model,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "done" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500 }
        })))
        .expect(times)
        .mount(server)
        .await;
}

async fn forbid_completions(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .named("a model outside the product policy is never called")
        .mount(server)
        .await;
}

async fn mount_smart_routing(aether: &MockServer, entitled: bool) {
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/entitlements/keystone/organization/org_1/smart_routing/check$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "hasEntitlement": entitled })))
        .mount(aether)
        .await;
}

/// The wallet is checked and charged at the cheap default's price: 1000 in at
/// $1000/Mtok plus 1000 (`max_tokens`) out at $2000/Mtok is $3 upstream, $6
/// retail; the actual 500 out is $2 upstream, $4 retail
async fn mount_wallet_for_cheap(aether: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/credits/keystone/organization/org_1/check"))
        .and(QueryApprox("amount", 6.0))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sufficient": true, "balance": 50.0
        })))
        .expect(1)
        .named("spend check priced on the product default")
        .mount(aether)
        .await;
    Mock::given(method("POST"))
        .and(path("/credits/keystone/organization/org_1/deduct"))
        .and(BodyApprox("amount", 4.0))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "balance": 46.0
        })))
        .expect(1)
        .named("deduct priced on the served default")
        .mount(aether)
        .await;
}

async fn mount_funded_wallet(aether: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/credits/keystone/organization/org_1/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sufficient": true, "balance": 50.0
        })))
        .mount(aether)
        .await;
    Mock::given(method("POST"))
        .and(path("/credits/keystone/organization/org_1/deduct"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "balance": 38.0
        })))
        .mount(aether)
        .await;
}

async fn forbid_wallet(aether: &MockServer) {
    Mock::given(path_regex(r"^/credits/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .named("a refused model never reaches the wallet")
        .mount(aether)
        .await;
}

/// The model and provider recorded on the request meter's usage event
async fn metered_model(aether: &MockServer) -> (String, String) {
    let url_path = "/usage/keystone/organization/org_1/requests/record";
    wait_for_request(aether, url_path).await;
    let recorded = aether
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|r| r.url.path() == url_path)
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&recorded.body).unwrap();
    (
        body["metadata"]["provider"].as_str().unwrap_or_default().to_owned(),
        body["metadata"]["model"].as_str().unwrap_or_default().to_owned(),
    )
}

#[tokio::test]
async fn auto_resolves_to_the_product_default_and_bills_it() {
    let servers = Servers::start().await;
    mount_completion(&servers.cheap, CHEAP, 1).await;
    forbid_completions(&servers.other).await;
    mount_smart_routing(&servers.aether, false).await;
    mount_usage_meters(&servers.aether).await;
    mount_wallet_for_cheap(&servers.aether).await;
    let state = llm_state(&servers).await;

    let response = state.complete(request("auto"), keystone()).await.unwrap();

    assert!((response.usage.unwrap().cost.unwrap().cost_usd - 4.0).abs() < 1e-9);
    assert_eq!(
        metered_model(&servers.aether).await,
        ("cheap".to_owned(), CHEAP.to_owned())
    );
}

#[tokio::test]
async fn every_virtual_model_falls_back_to_the_default_without_smart_routing() {
    for class in ["fast", "best", "cheap"] {
        let servers = Servers::start().await;
        mount_completion(&servers.cheap, CHEAP, 1).await;
        forbid_completions(&servers.other).await;
        forbid_completions(&servers.pricey).await;
        mount_smart_routing(&servers.aether, false).await;
        mount_usage_meters(&servers.aether).await;
        mount_wallet_for_cheap(&servers.aether).await;
        let state = llm_state(&servers).await;

        state
            .complete(request(class), keystone())
            .await
            .unwrap_or_else(|e| panic!("{class}: {e}"));
    }
}

#[tokio::test]
async fn smart_routing_routes_only_among_the_product_models() {
    let servers = Servers::start().await;
    // "best" picks the highest quality model; the other model scores higher
    // and is the configured threshold pick, but keystone may not use it
    mount_completion(&servers.pricey, PRICEY, 1).await;
    forbid_completions(&servers.other).await;
    mount_smart_routing(&servers.aether, true).await;
    mount_usage_meters(&servers.aether).await;
    mount_funded_wallet(&servers.aether).await;
    let state = llm_state(&servers).await;

    state.complete(request("best"), keystone()).await.unwrap();

    assert_eq!(
        metered_model(&servers.aether).await,
        ("pricey".to_owned(), PRICEY.to_owned())
    );
}

#[tokio::test]
async fn a_model_outside_the_policy_is_refused_before_any_spend() {
    for model in [format!("other/{OTHER}"), OTHER.to_owned(), "nope/unknown".to_owned()] {
        let servers = Servers::start().await;
        forbid_completions(&servers.other).await;
        forbid_wallet(&servers.aether).await;
        let state = llm_state(&servers).await;

        let error = state.complete(request(&model), keystone()).await.unwrap_err();

        assert!(matches!(error, LlmError::ModelNotAllowed { .. }), "{model}: {error}");
        assert_eq!(error.status_code(), http::StatusCode::BAD_REQUEST);
        assert_eq!(error.error_type(), "invalid_request_error");
        let message = error.client_message();
        assert!(message.contains(&model), "{message}");
        assert!(message.contains("GET /v1/models"), "{message}");
    }
}

#[tokio::test]
async fn an_allowed_model_is_served_by_bare_or_prefixed_id() {
    for model in [format!("cheap/{CHEAP}"), CHEAP.to_owned()] {
        let servers = Servers::start().await;
        mount_completion(&servers.cheap, CHEAP, 1).await;
        mount_usage_meters(&servers.aether).await;
        // A bare id may fail over to the pricey model, so its spend check is
        // priced there; only what served is charged
        mount_funded_wallet(&servers.aether).await;
        let state = llm_state(&servers).await;

        state
            .complete(request(&model), keystone())
            .await
            .unwrap_or_else(|e| panic!("{model}: {e}"));
    }
}

#[tokio::test]
async fn failover_skips_a_model_outside_the_policy_and_bills_the_one_served() {
    let servers = Servers::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_string("overloaded"))
        .mount(&servers.cheap)
        .await;
    // The other model is next in the group, but keystone may not use it
    forbid_completions(&servers.other).await;
    mount_completion(&servers.pricey, PRICEY, 1).await;
    mount_usage_meters(&servers.aether).await;
    // Checked on the pricey alternative (the most expensive reachable model,
    // the other model is not reachable): $9 upstream, $18 retail
    Mock::given(method("GET"))
        .and(path("/credits/keystone/organization/org_1/check"))
        .and(QueryApprox("amount", 18.0))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sufficient": true, "balance": 50.0
        })))
        .expect(1)
        .mount(&servers.aether)
        .await;
    Mock::given(method("POST"))
        .and(path("/credits/keystone/organization/org_1/deduct"))
        .and(BodyApprox("amount", 12.0))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "balance": 38.0
        })))
        .expect(1)
        .mount(&servers.aether)
        .await;
    let state = llm_state(&servers).await;

    // Bare id, so the request is not pinned to a provider and may fail over
    state.complete(request(CHEAP), keystone()).await.unwrap();

    assert_eq!(
        metered_model(&servers.aether).await,
        ("pricey".to_owned(), PRICEY.to_owned())
    );
}

#[tokio::test]
async fn a_product_key_lists_exactly_its_models_priced_for_it() {
    let servers = Servers::start().await;
    let state = llm_state(&servers).await;

    let listed = state.models_for(Some(&keystone())).await;

    let ids: Vec<&str> = listed.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, [format!("cheap/{CHEAP}"), format!("pricey/{PRICEY}")]);
    assert!(listed[0].default);
    assert!(!listed[1].default);
    // Retail credits at the 2.0 product margin: $1000/Mtok is 200,000 credits
    assert_eq!(
        listed[0].credits_per_mtok,
        Some(CreditsPerMtok {
            input: 200_000.0,
            output: 400_000.0
        })
    );
    assert_eq!(listed[1].display_name, "Pricey Model");
    assert_eq!(listed[0].display_name, "GPT Cheap");
    assert_eq!(listed[0].context_window, Some(200_000));
}

#[tokio::test]
async fn the_gateway_own_keys_are_not_restricted() {
    let servers = Servers::start().await;
    mount_completion(&servers.other, OTHER, 1).await;
    mount_usage_meters(&servers.aether).await;
    Mock::given(method("GET"))
        .and(path("/credits/synapse/user/usr_1/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sufficient": true, "balance": 50.0
        })))
        .mount(&servers.aether)
        .await;
    Mock::given(method("POST"))
        .and(path("/credits/synapse/user/usr_1/deduct"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "balance": 49.0
        })))
        .mount(&servers.aether)
        .await;
    let state = llm_state(&servers).await;

    state
        .complete(request(&format!("other/{OTHER}")), context("synapse", "user", "usr_1"))
        .await
        .unwrap();
}
