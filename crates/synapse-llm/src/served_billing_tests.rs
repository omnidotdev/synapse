//! Billing follows the model that served a request: failover to an alternate
//! and cascade escalation charge, meter, and report the model that produced
//! the response, never the one resolved before the request was sent

use std::collections::HashMap;

use futures_util::StreamExt;
use secrecy::SecretString;
use synapse_billing::{AetherClient, MeterKeys, UsageRecorder};
use synapse_config::{
    CascadeConfig, CircuitBreakerConfig, EquivalenceGroup, FailMode, FailoverConfig, LlmConfig, LlmProviderConfig,
    LlmProviderType, ModelConfig, ModelProfileConfig, RoutingStrategy,
};
use synapse_core::MarginPolicy;
use url::Url;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::spend_tests::{BodyApprox, QueryApprox, context, mount_usage_meters, wait_for_request};
use crate::state::LlmState;
use crate::types::{CompletionRequest, StreamEvent};

const CHEAP: &str = "gpt-test";
const PRICEY: &str = "gpt-pricey";

// A 4000-char prompt is 1000 input tokens, plus max_tokens 1000 output. At
// the pricey model's $3000/Mtok in and $6000/Mtok out that is $9 upstream,
// $18 retail at the 2.0 product margin (the cheap model would estimate $6)
const PRICEY_ESTIMATE_USD: f64 = 18.0;
// Actual usage of 1000 input + 500 output tokens on the pricey model: $6
// upstream, $12 retail (the cheap model would charge $4)
const PRICEY_ACTUAL_USD: f64 = 12.0;

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
        context_window: 200_000,
        input_per_mtok,
        output_per_mtok,
        quality,
        capabilities: synapse_config::ModelCapabilities::default(),
        long_context_pricing: None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Setup {
    Failover,
    Cascade,
    /// Failover to an alternate the gateway has no price for
    UnpricedAlternate,
}

/// Gateway with a cheap and a pricey provider in one equivalence group
async fn llm_state(cheap: &MockServer, pricey: &MockServer, aether: &MockServer, setup: Setup) -> LlmState {
    let mut config = LlmConfig::default();
    config
        .providers
        .insert("cheap".to_owned(), provider_config(cheap, CHEAP));
    config
        .providers
        .insert("pricey".to_owned(), provider_config(pricey, PRICEY));
    config.failover = FailoverConfig {
        enabled: true,
        max_attempts: 2,
        equivalence_groups: vec![EquivalenceGroup {
            name: "group".to_owned(),
            models: vec![format!("cheap/{CHEAP}"), format!("pricey/{PRICEY}")],
        }],
        circuit_breaker: CircuitBreakerConfig {
            error_threshold: 100,
            window_seconds: 60,
            recovery_seconds: 30,
        },
    };
    config.routing.models = vec![profile("cheap", CHEAP, 1000.0, 2000.0, 0.5)];
    if setup != Setup::UnpricedAlternate {
        config
            .routing
            .models
            .push(profile("pricey", PRICEY, 3000.0, 6000.0, 0.9));
    }
    if setup == Setup::Cascade {
        config.routing.enabled = true;
        config.routing.strategy = RoutingStrategy::Cascade;
        config.routing.cascade = CascadeConfig {
            initial_model: Some(format!("cheap/{CHEAP}")),
            escalation_model: Some(format!("pricey/{PRICEY}")),
            ..CascadeConfig::default()
        };
    }

    let mut state = LlmState::from_config(config).await.unwrap();
    state.set_margin_policy(MarginPolicy {
        default_app_id: "synapse".to_owned(),
        provider_margins: HashMap::new(),
        tier_margins: HashMap::new(),
        product_margins: HashMap::from([("facet".to_owned(), 2.0)]),
        default_product_margin: 2.0,
    });

    let client = AetherClient::new(
        Url::parse(&format!("{}/", aether.uri())).unwrap(),
        SecretString::from("aether-key".to_owned()),
    )
    .unwrap();
    state.set_usage_recorder(UsageRecorder::new(client.clone(), MeterKeys::default()));
    state.set_billing_client(client, FailMode::Closed);
    state
}

fn request(model: &str, stream: bool) -> CompletionRequest {
    serde_json::from_value(serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": "x".repeat(4000) }],
        "params": { "max_tokens": 1000 },
        "stream": stream,
    }))
    .unwrap()
}

async fn mount_failing(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_string("overloaded"))
        .mount(server)
        .await;
}

async fn mount_completion(server: &MockServer, model: &str) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "c1",
            "object": "chat.completion",
            "created": 1,
            "model": model,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "groceries" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500 }
        })))
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_streamed_completion(server: &MockServer, model: &str) {
    let chunk = serde_json::json!({
        "id": "c1", "object": "chat.completion.chunk", "created": 1, "model": model,
        "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "groceries" }, "finish_reason": "stop" }]
    });
    let usage = serde_json::json!({
        "id": "c1", "object": "chat.completion.chunk", "created": 1, "model": model,
        "choices": [],
        "usage": { "prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500 }
    });
    let body = format!("data: {chunk}\n\ndata: {usage}\n\ndata: [DONE]\n\n");

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .expect(1)
        .mount(server)
        .await;
}

/// The wallet check is priced on the most expensive model the request can
/// reach, and the deduct on the model that served it
async fn mount_wallet_for_pricey(aether: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/credits/facet/user/usr_1/check"))
        .and(QueryApprox("amount", PRICEY_ESTIMATE_USD))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sufficient": true, "balance": 50.0
        })))
        .expect(1)
        .named("spend check priced on the pricier reachable model")
        .mount(aether)
        .await;
    Mock::given(method("POST"))
        .and(path("/credits/facet/user/usr_1/deduct"))
        .and(BodyApprox("amount", PRICEY_ACTUAL_USD))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "balance": 38.0
        })))
        .expect(1)
        .named("deduct priced on the served model")
        .mount(aether)
        .await;
}

/// The model and provider recorded on the request meter's usage event
async fn metered_model(aether: &MockServer) -> (String, String) {
    let url_path = "/usage/facet/user/usr_1/requests/record";
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

async fn drain(mut stream: impl futures_util::Stream<Item = Result<StreamEvent, crate::LlmError>> + Unpin) -> bool {
    let mut saw_usage = false;
    while let Some(event) = stream.next().await {
        saw_usage |= matches!(event, Ok(StreamEvent::Usage(_)));
    }
    saw_usage
}

#[tokio::test]
async fn failover_to_a_pricier_model_bills_the_served_model() {
    let (cheap, pricey, aether) = (
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    );
    mount_failing(&cheap).await;
    mount_completion(&pricey, PRICEY).await;
    mount_wallet_for_pricey(&aether).await;
    mount_usage_meters(&aether).await;

    let state = llm_state(&cheap, &pricey, &aether, Setup::Failover).await;

    state
        .complete(request(CHEAP, false), context("facet", "user", "usr_1"))
        .await
        .unwrap();

    assert_eq!(metered_model(&aether).await, ("pricey".to_owned(), PRICEY.to_owned()));
}

#[tokio::test]
async fn streamed_failover_to_a_pricier_model_bills_the_served_model() {
    let (cheap, pricey, aether) = (
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    );
    mount_failing(&cheap).await;
    mount_streamed_completion(&pricey, PRICEY).await;
    mount_wallet_for_pricey(&aether).await;
    mount_usage_meters(&aether).await;

    let state = llm_state(&cheap, &pricey, &aether, Setup::Failover).await;

    let (served, stream) = state
        .complete_stream(request(CHEAP, true), context("facet", "user", "usr_1"))
        .await
        .unwrap();

    assert_eq!(served, PRICEY);
    assert!(drain(stream).await);
    wait_for_request(&aether, "/credits/facet/user/usr_1/deduct").await;
    assert_eq!(metered_model(&aether).await, ("pricey".to_owned(), PRICEY.to_owned()));
}

#[tokio::test]
async fn cascade_escalation_bills_the_escalated_model() {
    let (cheap, pricey, aether) = (
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    );
    // A one-word answer to a long prompt is low confidence, so it escalates
    mount_streamed_completion(&cheap, CHEAP).await;
    mount_streamed_completion(&pricey, PRICEY).await;
    mount_wallet_for_pricey(&aether).await;
    mount_usage_meters(&aether).await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/entitlements/facet/user/usr_1/smart_routing/check$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "hasEntitlement": true })))
        .mount(&aether)
        .await;

    let state = llm_state(&cheap, &pricey, &aether, Setup::Cascade).await;

    let (served, stream) = state
        .complete_stream(request("auto", true), context("facet", "user", "usr_1"))
        .await
        .unwrap();

    assert_eq!(served, PRICEY);
    assert!(drain(stream).await);
    wait_for_request(&aether, "/credits/facet/user/usr_1/deduct").await;
    assert_eq!(metered_model(&aether).await, ("pricey".to_owned(), PRICEY.to_owned()));
}

#[tokio::test]
async fn a_primary_success_bills_the_primary_model() {
    let (cheap, pricey, aether) = (
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    );
    mount_completion(&cheap, CHEAP).await;
    mount_usage_meters(&aether).await;
    // Still checked on the pricier reachable model, deducted on the cheap one
    // that served: 1000 in at $1000/Mtok + 500 out at $2000/Mtok, times 2.0
    Mock::given(method("GET"))
        .and(path("/credits/facet/user/usr_1/check"))
        .and(QueryApprox("amount", PRICEY_ESTIMATE_USD))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sufficient": true, "balance": 50.0
        })))
        .expect(1)
        .mount(&aether)
        .await;
    Mock::given(method("POST"))
        .and(path("/credits/facet/user/usr_1/deduct"))
        .and(BodyApprox("amount", 4.0))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "balance": 46.0
        })))
        .expect(1)
        .mount(&aether)
        .await;

    let state = llm_state(&cheap, &pricey, &aether, Setup::Failover).await;

    state
        .complete(request(CHEAP, false), context("facet", "user", "usr_1"))
        .await
        .unwrap();

    assert_eq!(metered_model(&aether).await, ("cheap".to_owned(), CHEAP.to_owned()));
}

#[tokio::test]
async fn a_managed_request_never_fails_over_to_an_unpriced_model() {
    let (cheap, pricey, aether) = (
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    );
    mount_failing(&cheap).await;
    // Serving the unpriced alternate on the managed key would bill nothing
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&pricey)
        .await;
    Mock::given(method("GET"))
        .and(path("/credits/facet/user/usr_1/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sufficient": true, "balance": 50.0
        })))
        .mount(&aether)
        .await;
    Mock::given(method("POST"))
        .and(path("/credits/facet/user/usr_1/deduct"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&aether)
        .await;

    let state = llm_state(&cheap, &pricey, &aether, Setup::UnpricedAlternate).await;

    let result = state
        .complete(request(CHEAP, false), context("facet", "user", "usr_1"))
        .await;

    assert!(result.is_err(), "the primary error surfaces instead");
}
