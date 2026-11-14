//! Thinking and effort reach Anthropic upstreams, and thinking tokens are
//! billed: Anthropic counts them in `output_tokens`, which the gateway charges
//! in full whether or not a thinking block is returned

use std::collections::HashMap;

use futures_util::StreamExt;
use secrecy::SecretString;
use synapse_billing::{AetherClient, MeterKeys, UsageRecorder};
use synapse_config::{FailMode, LlmConfig, LlmProviderConfig, LlmProviderType, ModelConfig, ModelProfileConfig};
use synapse_core::MarginPolicy;
use url::Url;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::protocol::anthropic::AnthropicRequest;
use crate::spend_tests::{BodyApprox, context, mount_usage_meters, wait_for_request};
use crate::state::LlmState;
use crate::types::{CompletionRequest, StreamEvent};

const MODEL: &str = "claude-test";

// 100 input tokens + 3000 output tokens (2900 of them thinking) at $1000/Mtok
// in and $2000/Mtok out: $6.10 upstream, $12.20 retail at the 2.0 margin
const THINKING_ACTUAL_USD: f64 = 12.2;

async fn llm_state(provider: &MockServer, aether: &MockServer) -> LlmState {
    let mut config = LlmConfig::default();
    config.providers.insert(
        "anthropic".to_owned(),
        LlmProviderConfig {
            provider_type: LlmProviderType::Anthropic,
            api_key: Some(SecretString::from("provider-key")),
            base_url: Some(format!("{}/v1", provider.uri()).parse().unwrap()),
            models: ModelConfig::default(),
            headers: Vec::new(),
            forward_authorization: false,
            rate_limit: None,
        },
    );
    config.routing.models = vec![ModelProfileConfig {
        provider: "anthropic".to_owned(),
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

/// An Anthropic-format request as a client sends it to `/v1/messages`
fn anthropic_request(stream: bool) -> CompletionRequest {
    let wire: AnthropicRequest = serde_json::from_value(serde_json::json!({
        "model": format!("anthropic/{MODEL}"),
        "max_tokens": 4000,
        "messages": [{ "role": "user", "content": "prove it" }],
        "thinking": { "type": "adaptive", "display": "summarized" },
        "output_config": { "effort": "high" },
        "stream": stream,
    }))
    .unwrap();
    wire.into()
}

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
        .and(BodyApprox("amount", THINKING_ACTUAL_USD))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "balance": 37.8
        })))
        .expect(1)
        .named("deduct covers every output token, thinking included")
        .mount(aether)
        .await;
}

/// The upstream only answers when thinking and effort arrived unchanged
fn expects_thinking_and_effort() -> impl wiremock::Match {
    body_partial_json(serde_json::json!({
        "thinking": { "type": "adaptive", "display": "summarized" },
        "output_config": { "effort": "high" },
    }))
}

#[tokio::test]
async fn thinking_and_effort_reach_anthropic_and_thinking_tokens_are_billed() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(expects_thinking_and_effort())
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": MODEL,
            "content": [
                { "type": "thinking", "thinking": "a summary", "signature": "sig" },
                { "type": "text", "text": "done" }
            ],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 100, "output_tokens": 3000 }
        })))
        .expect(1)
        .mount(&provider)
        .await;
    mount_wallet(&aether).await;
    mount_usage_meters(&aether).await;

    let response = llm_state(&provider, &aether)
        .await
        .complete(anthropic_request(false), context("facet", "user", "usr_1"))
        .await
        .unwrap();

    assert_eq!(response.usage.map(|u| u.completion_tokens), Some(3000));
}

#[tokio::test]
async fn streamed_thinking_tokens_are_billed() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    let events = [
        serde_json::json!({ "type": "message_start", "message": {
            "id": "msg_1", "type": "message", "role": "assistant", "model": MODEL,
            "usage": { "input_tokens": 100, "output_tokens": 1 }
        }}),
        serde_json::json!({ "type": "content_block_start", "index": 0,
            "content_block": { "type": "thinking", "thinking": "" } }),
        serde_json::json!({ "type": "content_block_delta", "index": 0,
            "delta": { "type": "thinking_delta", "thinking": "a summary" } }),
        serde_json::json!({ "type": "content_block_delta", "index": 0,
            "delta": { "type": "signature_delta", "signature": "sig" } }),
        serde_json::json!({ "type": "content_block_stop", "index": 0 }),
        serde_json::json!({ "type": "content_block_start", "index": 1,
            "content_block": { "type": "text", "text": "" } }),
        serde_json::json!({ "type": "content_block_delta", "index": 1,
            "delta": { "type": "text_delta", "text": "done" } }),
        serde_json::json!({ "type": "content_block_stop", "index": 1 }),
        serde_json::json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" },
            "usage": { "output_tokens": 3000 } }),
        serde_json::json!({ "type": "message_stop" }),
    ];
    let body = events.iter().fold(String::new(), |mut body, e| {
        use std::fmt::Write;
        let _ = write!(body, "event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap());
        body
    });
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(expects_thinking_and_effort())
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .expect(1)
        .mount(&provider)
        .await;
    mount_wallet(&aether).await;
    mount_usage_meters(&aether).await;

    let (_, mut stream) = llm_state(&provider, &aether)
        .await
        .complete_stream(anthropic_request(true), context("facet", "user", "usr_1"))
        .await
        .unwrap();
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        if let Ok(StreamEvent::Delta(delta)) = event {
            text.push_str(delta.content.as_deref().unwrap_or_default());
        }
    }

    assert_eq!(text, "done", "thinking is never mixed into the answer text");
    wait_for_request(&aether, "/credits/facet/user/usr_1/deduct").await;
}
