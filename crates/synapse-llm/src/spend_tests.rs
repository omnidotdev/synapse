//! End-to-end spend routing through `LlmState`: a product that includes AI in
//! its tier (Posture C, e.g. myfi) is checked against and charged to its
//! allowance meter in credits and never touches the credit wallet, while a
//! credit product (Posture A, e.g. facet) is unchanged

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use secrecy::SecretString;
use synapse_billing::{AetherClient, MeterKeys, SpendPolicy, UsageRecorder};
use synapse_config::{FailMode, LlmConfig, LlmProviderConfig, LlmProviderType, ModelConfig, ModelProfileConfig};
use synapse_core::{BillingIdentity, BillingMode, MarginPolicy, RequestContext};
use url::Url;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

use crate::error::LlmError;
use crate::state::LlmState;
use crate::types::{CompletionRequest, StreamEvent};

pub const MODEL: &str = "gpt-test";

// Request estimate: a 4000-char prompt is 1000 input tokens, plus max_tokens
// 1000 output, at $1000/Mtok in and $2000/Mtok out: $3 upstream, $6 retail at
// the 2.0 product margin, 600 credits
pub const ESTIMATED_CREDITS: f64 = 600.0;
// Actual usage: 1000 input + 500 output tokens: $2 upstream, $4 retail, 400 credits
pub const ACTUAL_CREDITS: f64 = 400.0;
pub const ACTUAL_USD: f64 = 4.0;

/// Matches a numeric query parameter within float tolerance
pub struct QueryApprox(pub &'static str, pub f64);

impl Match for QueryApprox {
    fn matches(&self, request: &Request) -> bool {
        request
            .url
            .query_pairs()
            .find(|(k, _)| k == self.0)
            .and_then(|(_, v)| v.parse::<f64>().ok())
            .is_some_and(|v| (v - self.1).abs() < 1e-6)
    }
}

/// Matches a numeric top-level JSON body field within float tolerance
pub struct BodyApprox(pub &'static str, pub f64);

impl Match for BodyApprox {
    fn matches(&self, request: &Request) -> bool {
        serde_json::from_slice::<serde_json::Value>(&request.body)
            .ok()
            .and_then(|body| body.get(self.0).and_then(serde_json::Value::as_f64))
            .is_some_and(|v| (v - self.1).abs() < 1e-6)
    }
}

pub async fn llm_state(provider: &MockServer, aether: &MockServer, fail_mode: FailMode) -> LlmState {
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
        input_per_mtok: 1000.0,
        output_per_mtok: 2000.0,
        quality: 0.9,
        capabilities: synapse_config::ModelCapabilities::default(),
        long_context_pricing: None,
    }];

    let mut state = LlmState::from_config(config).await.unwrap();
    state.set_margin_policy(MarginPolicy {
        default_app_id: "synapse".to_owned(),
        provider_margins: HashMap::new(),
        tier_margins: HashMap::new(),
        product_margins: HashMap::from([("myfi".to_owned(), 2.0), ("facet".to_owned(), 2.0)]),
        default_product_margin: 2.0,
    });

    let client = AetherClient::new(
        Url::parse(&format!("{}/", aether.uri())).unwrap(),
        SecretString::from("aether-key".to_owned()),
    )
    .unwrap()
    .with_spend_policy(SpendPolicy::new(HashMap::from([(
        "myfi".to_owned(),
        "ai_credits".to_owned(),
    )])));
    state.set_usage_recorder(UsageRecorder::new(client.clone(), MeterKeys::default()));
    state.set_billing_client(client, fail_mode);
    state
}

pub fn context(app_id: &str, entity_type: &str, entity_id: &str) -> RequestContext {
    let mut ctx = RequestContext::empty();
    ctx.billing_identity = Some(BillingIdentity {
        app_id: app_id.to_owned(),
        entity_type: entity_type.to_owned(),
        entity_id: entity_id.to_owned(),
        mode: BillingMode::Managed,
        plan: Some("pro".to_owned()),
    });
    ctx
}

pub fn request(stream: bool) -> CompletionRequest {
    serde_json::from_value(serde_json::json!({
        "model": format!("mock/{MODEL}"),
        "messages": [{ "role": "user", "content": "x".repeat(4000) }],
        "params": { "max_tokens": 1000 },
        "stream": stream,
    }))
    .unwrap()
}

pub async fn mount_completion(provider: &MockServer, times: u64) {
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
            "usage": { "prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500 }
        })))
        .expect(times)
        .mount(provider)
        .await;
}

pub async fn mount_streamed_completion(provider: &MockServer) {
    let chunk = serde_json::json!({
        "id": "c1", "object": "chat.completion.chunk", "created": 1, "model": MODEL,
        "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "groceries" }, "finish_reason": "stop" }]
    });
    let usage = serde_json::json!({
        "id": "c1", "object": "chat.completion.chunk", "created": 1, "model": MODEL,
        "choices": [],
        "usage": { "prompt_tokens": 1000, "completion_tokens": 500, "total_tokens": 1500 }
    });
    let body = format!("data: {chunk}\n\ndata: {usage}\n\ndata: [DONE]\n\n");

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .expect(1)
        .mount(provider)
        .await;
}

/// The token and request meters every managed request records, as today
pub async fn mount_usage_meters(aether: &MockServer) {
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/usage/[a-z]+/[a-z]+/[a-z0-9_]+/(input_tokens|output_tokens|requests|messages)/record$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "eventId": "e" })))
        .mount(aether)
        .await;
}

pub async fn forbid_wallet(aether: &MockServer) {
    Mock::given(path_regex(r"^/credits/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .named("an included-allowance product must never touch the credit wallet")
        .mount(aether)
        .await;
}

async fn mount_allowance_check(aether: &MockServer, allowed: bool) {
    Mock::given(method("GET"))
        .and(path("/usage/myfi/organization/org_1/ai_credits/check"))
        .and(QueryApprox("additionalUsage", ESTIMATED_CREDITS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "allowed": allowed, "current": 0, "limit": 1000
        })))
        .expect(1)
        .named("allowance check with the estimate in credits")
        .mount(aether)
        .await;
}

async fn mount_allowance_record(aether: &MockServer, times: u64) {
    Mock::given(method("POST"))
        .and(path("/usage/myfi/organization/org_1/ai_credits/record"))
        .and(BodyApprox("delta", ACTUAL_CREDITS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "eventId": "e" })))
        .expect(times)
        .named("allowance record with the actual cost in credits")
        .mount(aether)
        .await;
}

/// Wait for a fire-and-forget request to reach the mock
pub async fn wait_for_request(server: &MockServer, url_path: &str) {
    for _ in 0..100 {
        let seen = server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .any(|r| r.url.path() == url_path);
        if seen {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no request to {url_path}");
}

#[tokio::test]
async fn allowance_product_is_checked_and_charged_in_credits_never_the_wallet() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 1).await;
    forbid_wallet(&aether).await;
    mount_allowance_check(&aether, true).await;
    mount_allowance_record(&aether, 1).await;
    mount_usage_meters(&aether).await;

    let state = llm_state(&provider, &aether, FailMode::Degraded).await;

    let response = state
        .complete(request(false), context("myfi", "organization", "org_1"))
        .await
        .unwrap();

    assert!(response.usage.is_some());
    // Token and request meters still record as for every managed request
    wait_for_request(&aether, "/usage/myfi/organization/org_1/requests/record").await;
}

#[tokio::test]
async fn exhausted_allowance_is_refused_with_402_before_the_provider_is_called() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 0).await;
    forbid_wallet(&aether).await;
    mount_allowance_check(&aether, false).await;
    mount_allowance_record(&aether, 0).await;

    let state = llm_state(&provider, &aether, FailMode::Degraded).await;

    let err = state
        .complete(request(false), context("myfi", "organization", "org_1"))
        .await
        .unwrap_err();

    assert!(matches!(err, LlmError::AllowanceExhausted));
    assert_eq!(
        synapse_core::HttpError::status_code(&err),
        http::StatusCode::PAYMENT_REQUIRED
    );
    assert!(!err.is_retryable(), "a 402 must never fail over to a backup model");
}

#[tokio::test]
async fn streamed_allowance_usage_is_charged_in_credits_never_the_wallet() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_streamed_completion(&provider).await;
    forbid_wallet(&aether).await;
    mount_allowance_check(&aether, true).await;
    mount_allowance_record(&aether, 1).await;
    mount_usage_meters(&aether).await;

    let state = llm_state(&provider, &aether, FailMode::Degraded).await;

    let (_, mut stream) = state
        .complete_stream(request(true), context("myfi", "organization", "org_1"))
        .await
        .unwrap();
    let mut saw_usage = false;
    while let Some(event) = stream.next().await {
        saw_usage |= matches!(event, Ok(StreamEvent::Usage(_)));
    }

    assert!(saw_usage);
    wait_for_request(&aether, "/usage/myfi/organization/org_1/ai_credits/record").await;
}

#[tokio::test]
async fn streamed_exhausted_allowance_is_refused_before_streaming() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 0).await;
    forbid_wallet(&aether).await;
    mount_allowance_check(&aether, false).await;

    let state = llm_state(&provider, &aether, FailMode::Degraded).await;

    let result = state
        .complete_stream(request(true), context("myfi", "organization", "org_1"))
        .await;

    assert!(matches!(result, Err(LlmError::AllowanceExhausted)));
}

#[tokio::test]
async fn an_unreachable_allowance_check_follows_the_fail_mode() {
    for (fail_mode, served) in [
        (FailMode::Degraded, true),
        (FailMode::Open, true),
        (FailMode::Closed, false),
    ] {
        let (provider, aether) = (MockServer::start().await, MockServer::start().await);
        mount_completion(&provider, u64::from(served)).await;
        forbid_wallet(&aether).await;
        mount_usage_meters(&aether).await;
        Mock::given(method("GET"))
            .and(path("/usage/myfi/organization/org_1/ai_credits/check"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&aether)
            .await;
        mount_allowance_record(&aether, u64::from(served)).await;

        let state = llm_state(&provider, &aether, fail_mode.clone()).await;

        let result = state
            .complete(request(false), context("myfi", "organization", "org_1"))
            .await;

        if served {
            assert!(result.is_ok(), "{fail_mode:?} must serve like the wallet path");
        } else {
            assert!(matches!(result, Err(LlmError::BillingUnavailable)));
        }
    }
}

#[tokio::test]
async fn wallet_product_is_unchanged() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 1).await;
    mount_usage_meters(&aether).await;
    Mock::given(path_regex(r"/ai_credits/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .named("a credit product must not use an allowance meter")
        .mount(&aether)
        .await;
    Mock::given(method("GET"))
        .and(path("/credits/facet/user/usr_1/check"))
        .and(QueryApprox("amount", ESTIMATED_CREDITS / 100.0))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sufficient": true, "balance": 50.0
        })))
        .expect(1)
        .mount(&aether)
        .await;
    Mock::given(method("POST"))
        .and(path("/credits/facet/user/usr_1/deduct"))
        .and(BodyApprox("amount", ACTUAL_USD))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "balance": 46.0
        })))
        .expect(1)
        .mount(&aether)
        .await;

    let state = llm_state(&provider, &aether, FailMode::Degraded).await;

    state
        .complete(request(false), context("facet", "user", "usr_1"))
        .await
        .unwrap();
}

#[tokio::test]
async fn wallet_product_short_on_credits_is_still_insufficient_credits() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 0).await;
    Mock::given(method("GET"))
        .and(path("/credits/facet/user/usr_1/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sufficient": false, "balance": 0.5
        })))
        .expect(1)
        .mount(&aether)
        .await;

    let state = llm_state(&provider, &aether, FailMode::Degraded).await;

    let err = state
        .complete(request(false), context("facet", "user", "usr_1"))
        .await
        .unwrap_err();

    assert!(matches!(err, LlmError::InsufficientCredits { .. }));
}
