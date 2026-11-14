//! A product-scoped key gets its model picker and default from the gateway:
//! `GET /v1/models` lists exactly its product's models with display names,
//! the default flag, and retail prices; `auto` resolves to the default and
//! bills it; a model outside the product's policy is refused with a 400 in
//! the caller's protocol. A direct Synapse key keeps seeing every model

mod harness;

use std::time::Duration;

use harness::config::ConfigBuilder;
use harness::mock_llm::MockLlm;
use harness::server::TestServer;
use synapse_config::{ModelCapabilities, ModelProfileConfig, ProductModelPolicyConfig};
use wiremock::matchers::{body_partial_json, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEYSTONE_KEY: &str = "synapse_keystone_key";
const DIRECT_KEY: &str = "synapse_direct_key";

fn resolved_key(app_id: Option<&str>, entity: (&str, &str)) -> serde_json::Value {
    serde_json::json!({
        "userId": "11111111-1111-4111-8111-111111111111",
        "workspaceId": null,
        "apiKeyId": "22222222-2222-4222-8222-222222222222",
        "mode": "managed",
        "plan": "pro",
        "appId": app_id,
        "billingEntity": { "entityType": entity.0, "entityId": entity.1 },
        "providerKeys": []
    })
}

async fn mock_api() -> MockServer {
    let api = MockServer::start().await;
    for (key, body) in [
        (KEYSTONE_KEY, resolved_key(Some("keystone"), ("organization", "org_1"))),
        (DIRECT_KEY, resolved_key(None, ("user", "u1"))),
    ] {
        Mock::given(method("POST"))
            .and(path("/internal/resolve-key"))
            .and(body_partial_json(serde_json::json!({ "key": key })))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&api)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/internal/report-usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "recorded": 1, "skipped": 0 })))
        .mount(&api)
        .await;
    api
}

/// Aether granting every check except smart routing, with funded wallets
async fn mock_aether() -> MockServer {
    let aether = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/entitlements/.+/smart_routing/check$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "hasEntitlement": false })))
        .with_priority(1)
        .mount(&aether)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/entitlements/.+/check$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "hasEntitlement": true })))
        .mount(&aether)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/entitlements/[^/]+/[^/]+/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "entitlements": [] })))
        .mount(&aether)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/usage/.+/check$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "allowed": true })))
        .mount(&aether)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/usage/.+/record$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "eventId": "e" })))
        .mount(&aether)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/credits/.+/check$"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "sufficient": true, "balance": 10.0 })),
        )
        .mount(&aether)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/credits/.+/deduct$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "success": true })))
        .mount(&aether)
        .await;
    aether
}

fn profile(model: &str, input_per_mtok: f64, output_per_mtok: f64, display_name: Option<&str>) -> ModelProfileConfig {
    ModelProfileConfig {
        provider: "mock".to_owned(),
        model: model.to_owned(),
        display_name: display_name.map(str::to_owned),
        context_window: 128_000,
        input_per_mtok,
        output_per_mtok,
        quality: 0.9,
        capabilities: ModelCapabilities::default(),
        long_context_pricing: None,
    }
}

/// Keystone may use gpt-test (its default) and gpt-big; the mock also serves
/// mock-model-1, which keystone may not use
async fn server(mock: &MockLlm, api: &MockServer, aether: &MockServer) -> TestServer {
    let mut config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .with_auth(&api.uri())
        .with_billing(&aether.uri())
        .build();
    config.llm.routing.models = vec![
        profile("gpt-test", 30.0, 60.0, Some("GPT Test")),
        profile("gpt-big", 60.0, 120.0, None),
        profile("mock-model-1", 1.0, 2.0, None),
    ];
    let billing = config.billing.as_mut().unwrap();
    billing.products.insert(
        "keystone".to_owned(),
        ProductModelPolicyConfig {
            models: vec!["mock/gpt-test".to_owned(), "mock/gpt-big".to_owned()],
            default_model: "mock/gpt-test".to_owned(),
        },
    );
    config.validate().expect("valid config");
    TestServer::start(config).await.unwrap()
}

async fn list_models(server: &TestServer, key: &str) -> serde_json::Value {
    let response = server
        .client()
        .get(server.url("/v1/models"))
        .bearer_auth(key)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

#[tokio::test]
async fn a_product_key_lists_exactly_its_models_with_prices_and_default() {
    let mock = MockLlm::start().await.unwrap();
    let (api, aether) = (mock_api().await, mock_aether().await);
    let server = server(&mock, &api, &aether).await;

    let body = list_models(&server, KEYSTONE_KEY).await;

    assert_eq!(body["object"], "list");
    assert_eq!(
        body["data"],
        serde_json::json!([
            {
                "id": "mock/gpt-test",
                "object": "model",
                "created": body["data"][0]["created"],
                "owned_by": "synapse",
                "synapse": {
                    "display_name": "GPT Test",
                    "default": true,
                    "context_window": 128_000,
                    // $30 / $60 per Mtok at the 2.0 product margin
                    "credits_per_mtok": { "input": 6000.0, "output": 12000.0 }
                }
            },
            {
                "id": "mock/gpt-big",
                "object": "model",
                "created": body["data"][1]["created"],
                "owned_by": "synapse",
                "synapse": {
                    "display_name": "GPT Big",
                    "default": false,
                    "context_window": 128_000,
                    "credits_per_mtok": { "input": 12000.0, "output": 24000.0 }
                }
            }
        ])
    );
}

#[tokio::test]
async fn a_direct_key_sees_every_model_priced_at_its_tier() {
    let mock = MockLlm::start().await.unwrap();
    let (api, aether) = (mock_api().await, mock_aether().await);
    let server = server(&mock, &api, &aether).await;

    // Model discovery runs in the background
    let mut body = serde_json::Value::Null;
    for _ in 0..100 {
        body = list_models(&server, DIRECT_KEY).await;
        if body["data"].as_array().is_some_and(|data| !data.is_empty()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let listed = body["data"].as_array().unwrap();
    let discovered = listed.iter().find(|m| m["id"] == "mock/mock-model-1").unwrap();
    assert_eq!(discovered["synapse"]["default"], false);
    assert_eq!(discovered["synapse"]["display_name"], "Mock Model 1");
    // The gateway's own keys carry no product margin (no tier margin is
    // configured in the test harness, so the price is the upstream price)
    assert_eq!(
        discovered["synapse"]["credits_per_mtok"],
        serde_json::json!({ "input": 100.0, "output": 200.0 })
    );
}

async fn post(server: &TestServer, route: &str, body: serde_json::Value) -> reqwest::Response {
    server
        .client()
        .post(server.url(route))
        .bearer_auth(KEYSTONE_KEY)
        .json(&body)
        .send()
        .await
        .unwrap()
}

/// JSON bodies of every request the mock received on a path starting with
/// `prefix`
async fn bodies(mock: &MockServer, prefix: &str) -> Vec<serde_json::Value> {
    mock.received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path().starts_with(prefix))
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

#[tokio::test]
async fn auto_resolves_to_the_product_default_and_bills_it() {
    let mock = MockLlm::start().await.unwrap();
    let (api, aether) = (mock_api().await, mock_aether().await);
    let server = server(&mock, &api, &aether).await;

    let response = post(
        &server,
        "/v1/chat/completions",
        serde_json::json!({
            "model": "auto",
            "messages": [{ "role": "user", "content": "hi" }],
            "max_tokens": 16
        }),
    )
    .await;
    assert_eq!(response.status(), 200);
    // The mock's 10 in + 5 out at $30 / $60 per Mtok is $0.0006 upstream,
    // $0.0012 retail at the 2.0 product margin
    assert_eq!(response.headers()["x-synapse-cost-usd"], "0.0012");
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["model"], "gpt-test");

    // Charged to keystone's billing entity, on the default model
    let mut charges = Vec::new();
    for _ in 0..100 {
        charges = bodies(&aether, "/credits/keystone/organization/org_1/deduct").await;
        if !charges.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(charges.len(), 1);
    assert!((charges[0]["amount"].as_f64().unwrap() - 0.0012).abs() < 1e-9);
}

#[tokio::test]
async fn a_disallowed_model_is_a_400_in_each_protocol() {
    let mock = MockLlm::start().await.unwrap();
    let (api, aether) = (mock_api().await, mock_aether().await);
    let server = server(&mock, &api, &aether).await;

    let openai = post(
        &server,
        "/v1/chat/completions",
        serde_json::json!({
            "model": "mock/mock-model-1",
            "messages": [{ "role": "user", "content": "hi" }]
        }),
    )
    .await;
    assert_eq!(openai.status(), 400);
    let body: serde_json::Value = openai.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(
        body["error"]["message"],
        "model mock/mock-model-1 is not available for this API key; GET /v1/models lists the models it can use"
    );

    let anthropic = post(
        &server,
        "/v1/messages",
        serde_json::json!({
            "model": "mock/mock-model-1",
            "max_tokens": 16,
            "messages": [{ "role": "user", "content": "hi" }]
        }),
    )
    .await;
    assert_eq!(anthropic.status(), 400);
    let body: serde_json::Value = anthropic.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "invalid_request_error");

    assert_eq!(mock.completion_count(), 0, "the provider is never called");
    assert!(
        bodies(&aether, "/credits/").await.is_empty(),
        "the wallet is never touched"
    );
}
