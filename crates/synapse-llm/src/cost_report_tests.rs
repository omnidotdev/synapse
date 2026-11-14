//! Every billed response reports what it cost: response headers on a
//! non-streamed request, and `synapse_cost` on the final usage of the body or
//! stream, with the retail charge, its credits, and the upstream cost

use axum::body::Body;
use futures_util::StreamExt;
use secrecy::SecretString;
use synapse_config::FailMode;
use synapse_core::{BillingMode, RequestContext};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::handler::{COST_USD_HEADER, CREDITS_HEADER, PROVIDER_COST_USD_HEADER, llm_router};
use crate::spend_tests::{
    ACTUAL_USD, MODEL, context, llm_state, mount_completion, mount_streamed_completion, mount_usage_meters, request,
};
use crate::types::{RequestCost, StreamEvent};

// 1000 input + 500 output tokens at $1000/Mtok in and $2000/Mtok out
const UPSTREAM_USD: f64 = 2.0;

async fn mount_wallet(aether: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/credits/facet/user/usr_1/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sufficient": true, "balance": 50.0
        })))
        .mount(aether)
        .await;
    Mock::given(method("POST"))
        .and(path("/credits/facet/user/usr_1/deduct"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "balance": 46.0
        })))
        .mount(aether)
        .await;
}

fn byok_context() -> RequestContext {
    let mut ctx = context("facet", "user", "usr_1");
    if let Some(identity) = ctx.billing_identity.as_mut() {
        identity.mode = BillingMode::Byok;
    }
    ctx.provider_keys
        .insert("mock".to_owned(), SecretString::from("byok-key"));
    ctx
}

fn post(uri: &str, body: &serde_json::Value) -> http::Request<Body> {
    http::Request::post(uri)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn chat(stream: bool) -> serde_json::Value {
    serde_json::json!({
        "model": format!("mock/{MODEL}"),
        "messages": [{ "role": "user", "content": "x".repeat(4000) }],
        "max_tokens": 1000,
        "stream": stream,
    })
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn header<'a>(response: &'a axum::response::Response, name: &str) -> Option<&'a str> {
    response.headers().get(name).and_then(|v| v.to_str().ok())
}

#[tokio::test]
async fn a_completion_reports_its_retail_and_upstream_cost() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 1).await;
    mount_wallet(&aether).await;
    mount_usage_meters(&aether).await;

    let response = llm_state(&provider, &aether, FailMode::Closed)
        .await
        .complete(request(false), context("facet", "user", "usr_1"))
        .await
        .unwrap();

    assert_eq!(
        response.usage.and_then(|u| u.cost),
        Some(RequestCost::new(ACTUAL_USD, UPSTREAM_USD))
    );
}

#[tokio::test]
async fn a_non_streamed_response_carries_cost_headers_and_body_cost() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 1).await;
    mount_wallet(&aether).await;
    mount_usage_meters(&aether).await;
    let router = llm_router(llm_state(&provider, &aether, FailMode::Closed).await)
        .layer(axum::Extension(context("facet", "user", "usr_1")));

    let response = router
        .oneshot(post("/v1/chat/completions", &chat(false)))
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(header(&response, COST_USD_HEADER), Some("4"));
    assert_eq!(header(&response, CREDITS_HEADER), Some("400"));
    assert_eq!(header(&response, PROVIDER_COST_USD_HEADER), Some("2"));
    let body: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
    assert_eq!(
        body["usage"]["synapse_cost"],
        serde_json::json!({ "cost_usd": 4.0, "credits": 400.0, "provider_cost_usd": 2.0 })
    );
}

#[tokio::test]
async fn a_streamed_response_reports_cost_on_its_final_usage() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_streamed_completion(&provider).await;
    mount_wallet(&aether).await;
    mount_usage_meters(&aether).await;
    let router = llm_router(llm_state(&provider, &aether, FailMode::Closed).await)
        .layer(axum::Extension(context("facet", "user", "usr_1")));

    let response = router.oneshot(post("/v1/chat/completions", &chat(true))).await.unwrap();
    let sse = body_text(response).await;

    let usage_chunk = sse
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        .find(|chunk| chunk.get("usage").is_some_and(|u| !u.is_null()))
        .expect("a final usage chunk");
    assert_eq!(usage_chunk["usage"]["synapse_cost"]["cost_usd"], 4.0);
    assert_eq!(usage_chunk["usage"]["synapse_cost"]["provider_cost_usd"], 2.0);
}

#[tokio::test]
async fn an_anthropic_stream_reports_cost_on_message_delta_usage() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_streamed_completion(&provider).await;
    mount_wallet(&aether).await;
    mount_usage_meters(&aether).await;
    let router = llm_router(llm_state(&provider, &aether, FailMode::Closed).await)
        .layer(axum::Extension(context("facet", "user", "usr_1")));

    let body = serde_json::json!({
        "model": format!("mock/{MODEL}"),
        "max_tokens": 1000,
        "messages": [{ "role": "user", "content": "x".repeat(4000) }],
        "stream": true,
    });
    let sse = body_text(router.oneshot(post("/v1/messages", &body)).await.unwrap()).await;

    let delta = sse
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        .rev()
        .find(|event| event["type"] == "message_delta" && !event["usage"].is_null())
        .expect("a message_delta event carrying usage");
    assert_eq!(delta["usage"]["synapse_cost"]["credits"], 400.0);
}

#[tokio::test]
async fn a_byok_request_reports_zero_cost() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 1).await;
    mount_usage_meters(&aether).await;
    let router =
        llm_router(llm_state(&provider, &aether, FailMode::Closed).await).layer(axum::Extension(byok_context()));

    let response = router
        .oneshot(post("/v1/chat/completions", &chat(false)))
        .await
        .unwrap();

    assert_eq!(header(&response, COST_USD_HEADER), Some("0"));
    assert_eq!(header(&response, PROVIDER_COST_USD_HEADER), Some("0"));
}

#[tokio::test]
async fn an_unbilled_request_reports_no_cost() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 1).await;
    let router = llm_router(llm_state(&provider, &aether, FailMode::Closed).await)
        .layer(axum::Extension(RequestContext::empty()));

    let response = router
        .oneshot(post("/v1/chat/completions", &chat(false)))
        .await
        .unwrap();

    assert!(header(&response, COST_USD_HEADER).is_none());
}

#[tokio::test]
async fn a_streamed_state_event_carries_the_cost() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_streamed_completion(&provider).await;
    mount_wallet(&aether).await;
    mount_usage_meters(&aether).await;

    let (_, mut stream) = llm_state(&provider, &aether, FailMode::Closed)
        .await
        .complete_stream(request(true), context("facet", "user", "usr_1"))
        .await
        .unwrap();
    let mut cost = None;
    while let Some(event) = stream.next().await {
        if let Ok(StreamEvent::Usage(usage)) = event {
            cost = usage.cost;
        }
    }

    assert_eq!(cost, Some(RequestCost::new(ACTUAL_USD, UPSTREAM_USD)));
}
