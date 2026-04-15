mod harness;

use harness::config::ConfigBuilder;
use harness::mock_llm::MockLlm;
use harness::server::TestServer;

#[tokio::test]
async fn openai_chat_completion_returns_response() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let body = serde_json::json!({
        "model": "mock-model-1",
        "messages": [
            {"role": "user", "content": "Hello"}
        ]
    });

    let resp = server
        .client()
        .post(server.url("/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["object"], "chat.completion");
    assert!(json["choices"].is_array());
    assert_eq!(json["choices"][0]["message"]["role"], "assistant");
    assert_eq!(json["choices"][0]["message"]["content"], "Hello from mock LLM");
}

#[tokio::test]
async fn openai_chat_completion_unknown_provider_returns_error() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    // Use explicit provider/model format with a provider that doesn't exist
    let body = serde_json::json!({
        "model": "nonexistent-provider/some-model",
        "messages": [
            {"role": "user", "content": "Hello"}
        ]
    });

    let resp = server
        .client()
        .post(server.url("/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert!(
        resp.status().is_client_error(),
        "expected client error, got {}",
        resp.status()
    );
}

#[tokio::test]
async fn openai_list_models_returns_list() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let resp = server.client().get(server.url("/v1/models")).send().await.unwrap();

    assert_eq!(resp.status(), 200);

    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["object"], "list");
    assert!(json["data"].is_array());
}

#[tokio::test]
async fn mock_llm_tracks_completions() {
    let mock = MockLlm::start().await.unwrap();
    assert_eq!(mock.completion_count(), 0);

    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    // Make two completion requests
    for _ in 0..2 {
        let body = serde_json::json!({
            "model": "mock-model-1",
            "messages": [{"role": "user", "content": "Hi"}]
        });
        server
            .client()
            .post(server.url("/v1/chat/completions"))
            .json(&body)
            .send()
            .await
            .unwrap();
    }

    assert_eq!(mock.completion_count(), 2);
}
