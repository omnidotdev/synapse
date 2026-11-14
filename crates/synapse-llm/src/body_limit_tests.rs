//! Completion routes accept request bodies above axum's 2 MiB default, so a
//! multimodal request with inline images reaches the provider

use axum::body::Body;
use secrecy::SecretString;
use synapse_config::{LlmConfig, LlmProviderConfig, LlmProviderType, ModelConfig};
use synapse_core::RequestContext;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::handler::{REQUEST_BODY_LIMIT_BYTES, llm_router};
use crate::state::LlmState;

async fn router(provider: &MockServer) -> axum::Router {
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
    let state = LlmState::from_config(config).await.unwrap();
    llm_router(state).layer(axum::Extension(RequestContext::empty()))
}

async fn mount_completion(provider: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "c1",
            "object": "chat.completion",
            "created": 1,
            "model": "gpt-test",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "a cat" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
        })))
        .mount(provider)
        .await;
}

/// Completion requests the provider received (model discovery also calls it)
async fn completions_sent(provider: &MockServer) -> usize {
    provider
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path() == "/v1/chat/completions")
        .count()
}

/// A chat completion carrying an inline base64 image of `image_bytes`
fn completion_with_image(image_bytes: usize) -> http::Request<Body> {
    let image = "A".repeat(image_bytes);
    let body = serde_json::json!({
        "model": "mock/gpt-test",
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": "what is this" },
                { "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{image}") } }
            ]
        }],
        "max_tokens": 100
    });
    http::Request::post("/v1/chat/completions")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn a_request_above_the_2_mib_default_reaches_the_provider() {
    let provider = MockServer::start().await;
    mount_completion(&provider).await;

    let response = router(&provider)
        .await
        .oneshot(completion_with_image(3 << 20))
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(completions_sent(&provider).await, 1);
}

#[tokio::test]
async fn a_request_above_the_limit_is_refused_before_the_provider() {
    let provider = MockServer::start().await;
    mount_completion(&provider).await;

    let response = router(&provider)
        .await
        .oneshot(completion_with_image(REQUEST_BODY_LIMIT_BYTES))
        .await
        .unwrap();

    assert_eq!(response.status(), http::StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(completions_sent(&provider).await, 0);
}
