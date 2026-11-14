//! Every billed modality reports what it served to synapse-api, in the shape
//! synapse-api's report body schema accepts, carrying exactly what Aether was
//! charged. Aether's daily reconciliation compares the two per product, so a
//! modality that does not report (or reports a different amount) reads as
//! drift

mod harness;

use std::time::Duration;

use harness::config::ConfigBuilder;
use harness::mock_llm::MockLlm;
use harness::server::TestServer;
use synapse_config::{ModelCapabilities, ModelProfileConfig};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "synapse_test_key";

/// A Facet product key, outside any workspace, as synapse-api resolves it
fn facet_key() -> serde_json::Value {
    serde_json::json!({
        "userId": "11111111-1111-4111-8111-111111111111",
        "workspaceId": null,
        "apiKeyId": "22222222-2222-4222-8222-222222222222",
        "mode": "managed",
        "plan": "free",
        "appId": "facet",
        "billingEntity": { "entityType": "user", "entityId": "u1" },
        "providerKeys": []
    })
}

async fn mock_api() -> MockServer {
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/internal/resolve-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(facet_key()))
        .mount(&api)
        .await;
    Mock::given(method("POST"))
        .and(path("/internal/report-usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "recorded": 1, "skipped": 0 })))
        .mount(&api)
        .await;
    api
}

/// Aether granting every check, with a funded wallet
async fn mock_aether() -> MockServer {
    let aether = MockServer::start().await;
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
        .and(path("/credits/facet/user/u1/check"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "sufficient": true, "balance": 10.0 })),
        )
        .mount(&aether)
        .await;
    Mock::given(method("POST"))
        .and(path("/credits/facet/user/u1/deduct"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "success": true })))
        .mount(&aether)
        .await;
    aether
}

async fn server(mock: &MockLlm, api: &MockServer, aether: &MockServer) -> TestServer {
    let mut config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .with_embeddings_provider("mock", &mock.base_url())
        .with_imagegen_provider("mock", &mock.base_url())
        .with_auth(&api.uri())
        .with_billing(&aether.uri())
        .build();
    // $30 / $60 per Mtok: the mock's 10 + 5 tokens cost $0.0006 upstream,
    // $0.0012 at the 2.0 product margin
    config.llm.routing.models = vec![ModelProfileConfig {
        provider: "mock".to_owned(),
        model: "gpt-test".to_owned(),
        context_window: 128_000,
        input_per_mtok: 30.0,
        output_per_mtok: 60.0,
        quality: 0.9,
        capabilities: ModelCapabilities::default(),
        long_context_pricing: None,
    }];
    TestServer::start(config).await.unwrap()
}

async fn post(server: &TestServer, route: &str, body: serde_json::Value) {
    let response = server
        .client()
        .post(server.url(route))
        .bearer_auth(API_KEY)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{route}");
    // Drain a streamed body so its final usage is seen
    let _ = response.bytes().await.unwrap();
}

/// JSON bodies of every request the mock received on `route`
async fn bodies(mock: &MockServer, route: &str) -> Vec<serde_json::Value> {
    mock.received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path() == route)
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

/// Wait until `count` usage events reached synapse-api and `count` charges
/// reached Aether, then return both
async fn reports_and_charges(
    api: &MockServer,
    aether: &MockServer,
    count: usize,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    for _ in 0..300 {
        let events: Vec<serde_json::Value> = bodies(api, "/internal/report-usage")
            .await
            .into_iter()
            .flat_map(|body| body["events"].as_array().cloned().unwrap_or_default())
            .collect();
        let charges = bodies(aether, "/credits/facet/user/u1/deduct").await;
        if events.len() >= count && charges.len() >= count {
            return (events, charges);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {count} usage reports and charges");
}

/// synapse-api's body schema: no field may be null, ids are present, and
/// cost is a non-negative number of cents
fn assert_matches_report_schema(event: &serde_json::Value) {
    let fields = event.as_object().unwrap();
    assert!(fields.values().all(|v| !v.is_null()), "a field is null: {event}");
    assert!(!fields.contains_key("workspaceId"), "{event}");
    for key in [
        "eventId",
        "userId",
        "apiKeyId",
        "provider",
        "model",
        "mode",
        "modality",
        "occurredAt",
    ] {
        assert!(event[key].is_string(), "{key} must be a string: {event}");
    }
    for key in ["inputTokens", "outputTokens", "costCents"] {
        assert!(event[key].as_f64().is_some_and(|n| n >= 0.0), "{key}: {event}");
    }
}

#[tokio::test]
async fn every_modality_reports_what_it_was_charged() {
    let mock = MockLlm::start().await.unwrap();
    let (api, aether) = (mock_api().await, mock_aether().await);
    let server = server(&mock, &api, &aether).await;

    let chat = serde_json::json!({
        "model": "mock/gpt-test",
        "messages": [{ "role": "user", "content": "hi" }],
        "max_tokens": 16
    });
    post(&server, "/v1/chat/completions", chat.clone()).await;
    let mut streamed = chat;
    streamed["stream"] = serde_json::json!(true);
    post(&server, "/v1/chat/completions", streamed).await;
    post(
        &server,
        "/v1/embeddings",
        serde_json::json!({ "model": "mock/text-embedding-3-small", "input": "hello" }),
    )
    .await;
    post(
        &server,
        "/v1/images/generations",
        serde_json::json!({ "model": "mock/gpt-image-1.5", "prompt": "a fox", "n": 1, "size": "1024x1024" }),
    )
    .await;

    let (events, charges) = reports_and_charges(&api, &aether, 4).await;
    assert_eq!(events.len(), 4, "one report per served request");
    assert_eq!(charges.len(), 4, "one charge per served request");
    events.iter().for_each(assert_matches_report_schema);

    let modalities: Vec<&str> = events.iter().map(|e| e["modality"].as_str().unwrap()).collect();
    for modality in ["completion", "embedding", "image_generation"] {
        assert!(modalities.contains(&modality), "{modality} in {modalities:?}");
    }
    assert_eq!(modalities.iter().filter(|m| **m == "completion").count(), 2);

    // Each modality's reported cost is exactly what Aether was charged for it
    for reference_type in ["completion", "embedding", "image_generation"] {
        let reported: f64 = events
            .iter()
            .filter(|e| e["modality"] == reference_type)
            .map(|e| e["costCents"].as_f64().unwrap())
            .sum();
        let charged: f64 = charges
            .iter()
            .filter(|c| c["referenceType"] == reference_type)
            .map(|c| c["amount"].as_f64().unwrap() * 100.0)
            .sum();
        assert!(
            (reported - charged).abs() < 1e-9,
            "{reference_type}: reported {reported}c, charged {charged}c"
        );
        assert!(reported > 0.0, "{reference_type} is charged");
    }

    // A completion is $0.0012, i.e. 0.12 cents, not rounded to a whole cent
    let completion = events.iter().find(|e| e["modality"] == "completion").unwrap();
    assert_eq!(completion["costCents"], 0.12);
    assert_eq!(completion["mode"], "managed");
    assert_eq!(completion["model"], "gpt-test");
    // Embedding a few tokens costs less than the ledger unit, so it is
    // charged (and reported) one unit, $0.0001
    let embedding = events.iter().find(|e| e["modality"] == "embedding").unwrap();
    assert_eq!(embedding["costCents"], 0.01);
}
