//! A request authenticated by API key through the real auth and request
//! context middleware reaches what the LLM layer reads off the resolved key:
//! the dashboard usage report, and the response cache scope. Nothing else
//! authenticates a request: a JWT bearer is refused

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use http::Request;
use secrecy::SecretString;
use synapse_auth::{ApiKeyResolver, UsageReporter};
use synapse_config::{LlmConfig, LlmProviderConfig, LlmProviderType, ModelConfig, ModelProfileConfig};
use synapse_llm::LlmState;
use tower::ServiceExt;
use url::Url;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::{auth, request_context};

const MODEL: &str = "gpt-test";

/// synapse-api: resolves two keys owned by the same user, and accepts usage
/// reports
async fn mock_api() -> MockServer {
    let api = MockServer::start().await;
    for key in ["key_a", "key_b"] {
        Mock::given(method("POST"))
            .and(path("/internal/resolve-key"))
            .and(body_partial_json(
                serde_json::json!({ "key": format!("synapse_{key}") }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "userId": "usr_1",
                "workspaceId": null,
                "apiKeyId": key,
                "mode": "managed",
                "plan": "pro"
            })))
            .mount(&api)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/internal/report-usage"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&api)
        .await;
    api
}

async fn mount_completion(provider: &MockServer, times: u64) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "c1",
            "object": "chat.completion",
            "created": 1,
            "model": MODEL,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "groceries" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
        })))
        .expect(times)
        .mount(provider)
        .await;
}

async fn llm_state(provider: &MockServer) -> LlmState {
    let mut config = LlmConfig::default();
    config.providers.insert(
        "mock".to_owned(),
        LlmProviderConfig {
            provider_type: LlmProviderType::Openai,
            api_key: Some(SecretString::from("provider-key")),
            base_url: Some(format!("{}/v1", provider.uri()).parse().unwrap()),
            models: ModelConfig::default(),
            headers: Vec::new(),
            forward_authorization: false,
            rate_limit: None,
        },
    );
    config.routing.models = vec![ModelProfileConfig {
        provider: "mock".to_owned(),
        model: MODEL.to_owned(),
        context_window: 200_000,
        input_per_mtok: 1.0,
        output_per_mtok: 2.0,
        quality: 0.9,
        capabilities: synapse_config::ModelCapabilities::default(),
        long_context_pricing: None,
    }];

    let mut state = LlmState::from_config(config).await.unwrap();
    state.set_response_cache(synapse_cache::ResponseCache::in_memory(Duration::from_secs(3600)));
    state
}

/// The LLM routes behind the middleware the server wires for API key auth
fn router(state: LlmState, api: &MockServer) -> Router {
    let api_url = Url::parse(&api.uri()).unwrap();
    let secret = SecretString::from("gateway-secret");
    let resolver = ApiKeyResolver::new(api_url.clone(), secret.clone(), Duration::from_secs(60), 100, false).unwrap();
    let reporter = UsageReporter::spawn(api_url, secret, Duration::from_millis(50));

    synapse_llm::llm_router(state)
        .layer(axum::middleware::from_fn(request_context::request_context_middleware))
        .layer(axum::middleware::from_fn(move |req, next| {
            let resolver = resolver.clone();
            let reporter = Some(reporter.clone());
            async move { auth::auth_middleware(resolver, None, Vec::new(), reporter, "synapse".to_owned(), req, next).await }
        }))
}

async fn complete(router: &Router, key: &str) {
    let request = Request::post("/v1/chat/completions")
        .header(http::header::AUTHORIZATION, format!("Bearer synapse_{key}"))
        .header(http::header::CONTENT_TYPE, "application/json")
        .header(synapse_cache::OPT_IN_HEADER, "true")
        .body(Body::from(
            serde_json::json!({
                "model": format!("mock/{MODEL}"),
                "messages": [{ "role": "user", "content": "what should I buy" }],
                "max_tokens": 100
            })
            .to_string(),
        ))
        .unwrap();

    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);
}

/// Every usage event reported to synapse-api so far
async fn reported_events(api: &MockServer) -> Vec<serde_json::Value> {
    api.received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path() == "/internal/report-usage")
        .flat_map(|r| {
            serde_json::from_slice::<serde_json::Value>(&r.body).unwrap()["events"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

#[tokio::test]
async fn an_api_key_request_reaches_the_dashboard_usage_report() {
    let (provider, api) = (MockServer::start().await, mock_api().await);
    mount_completion(&provider, 1).await;

    let router = router(llm_state(&provider).await, &api);
    complete(&router, "key_a").await;

    let mut events = Vec::new();
    for _ in 0..100 {
        events = reported_events(&api).await;
        if !events.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(events.len(), 1, "the served request is reported once");
    assert_eq!(events[0]["apiKeyId"], "key_a");
    assert_eq!(events[0]["userId"], "usr_1");
    assert_eq!(events[0]["model"], MODEL);
    assert_eq!(events[0]["inputTokens"], 10);
}

#[tokio::test]
async fn the_response_cache_scope_includes_the_api_key() {
    let (provider, api) = (MockServer::start().await, mock_api().await);
    // One live call per key, the repeat on key_a is a cache hit
    mount_completion(&provider, 2).await;

    let router = router(llm_state(&provider).await, &api);
    for key in ["key_a", "key_b", "key_a"] {
        complete(&router, key).await;
    }
}

/// The gateway authenticates by Synapse API key only. A JWT (or any other
/// non-key bearer) is refused before any provider call, so there is no
/// unattributed, unbilled path for a request that skips API key auth
#[tokio::test]
async fn a_jwt_bearer_is_refused_before_reaching_a_provider() {
    let (provider, api) = (MockServer::start().await, mock_api().await);
    mount_completion(&provider, 0).await;

    let router = router(llm_state(&provider).await, &api);
    let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1c3JfMSJ9.c2lnbmF0dXJl";
    let request = Request::post("/v1/chat/completions")
        .header(http::header::AUTHORIZATION, format!("Bearer {jwt}"))
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "model": format!("mock/{MODEL}"),
                "messages": [{ "role": "user", "content": "what should I buy" }],
                "max_tokens": 100
            })
            .to_string(),
        ))
        .unwrap();

    let response = router.oneshot(request).await.unwrap();

    assert_eq!(response.status(), http::StatusCode::UNAUTHORIZED);
}
