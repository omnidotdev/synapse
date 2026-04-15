//! End-to-end tests for all Synapse endpoints

mod harness;

use harness::config::ConfigBuilder;
use harness::mock_llm::MockLlm;
use harness::server::TestServer;

// -- Embeddings --

#[tokio::test]
async fn embeddings_returns_response() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .with_embeddings_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let body = serde_json::json!({
        "model": "mock/text-embedding-test",
        "input": "Hello world"
    });

    let resp = server
        .client()
        .post(server.url("/v1/embeddings"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["object"], "list");
    assert!(json["data"].is_array());

    let embeddings = json["data"].as_array().unwrap();
    assert!(!embeddings.is_empty());
    assert_eq!(embeddings[0]["object"], "embedding");
    assert!(embeddings[0]["embedding"].is_array());

    assert_eq!(mock.embedding_count(), 1);
}

#[tokio::test]
async fn embeddings_no_provider_returns_error() {
    let mock = MockLlm::start().await.unwrap();
    // Only configure LLM, no embeddings provider
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let body = serde_json::json!({
        "model": "text-embedding-test",
        "input": "Hello world"
    });

    let resp = server
        .client()
        .post(server.url("/v1/embeddings"))
        .json(&body)
        .send()
        .await
        .unwrap();

    // Should fail since no embeddings provider is configured
    assert!(
        resp.status().is_client_error() || resp.status().is_server_error(),
        "expected error status, got {}",
        resp.status()
    );
}

// -- Image generation --

#[tokio::test]
async fn imagegen_returns_response() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .with_imagegen_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let body = serde_json::json!({
        "model": "mock/dall-e-test",
        "prompt": "A test image"
    });

    let resp = server
        .client()
        .post(server.url("/v1/images/generations"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    let json: serde_json::Value = resp.json().await.unwrap();
    assert!(json["created"].is_number());
    assert!(json["data"].is_array());

    let images = json["data"].as_array().unwrap();
    assert!(!images.is_empty());
    assert!(images[0]["url"].is_string());

    assert_eq!(mock.imagegen_count(), 1);
}

#[tokio::test]
async fn imagegen_no_provider_returns_error() {
    let mock = MockLlm::start().await.unwrap();
    // Only configure LLM, no imagegen provider
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let body = serde_json::json!({
        "model": "dall-e-test",
        "prompt": "A test image"
    });

    let resp = server
        .client()
        .post(server.url("/v1/images/generations"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert!(
        resp.status().is_client_error() || resp.status().is_server_error(),
        "expected error status, got {}",
        resp.status()
    );
}

// -- Chat completions with provider routing --

#[tokio::test]
async fn chat_routes_to_correct_provider() {
    let primary = MockLlm::start_with_response("primary response").await.unwrap();
    let secondary = MockLlm::start_with_response("secondary response").await.unwrap();

    let config = ConfigBuilder::new()
        .with_openai_provider("primary", &primary.base_url())
        .with_openai_provider("secondary", &secondary.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    // Route to primary
    let body = serde_json::json!({
        "model": "primary/mock-model-1",
        "messages": [{"role": "user", "content": "Hello"}]
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
    assert_eq!(json["choices"][0]["message"]["content"], "primary response");
    assert_eq!(primary.completion_count(), 1);
    assert_eq!(secondary.completion_count(), 0);

    // Route to secondary
    let body = serde_json::json!({
        "model": "secondary/mock-model-1",
        "messages": [{"role": "user", "content": "Hello"}]
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
    assert_eq!(json["choices"][0]["message"]["content"], "secondary response");
    assert_eq!(secondary.completion_count(), 1);
}

// -- Models --

#[tokio::test]
async fn models_lists_from_all_providers() {
    let provider_a = MockLlm::start().await.unwrap();
    let provider_b = MockLlm::start().await.unwrap();

    let config = ConfigBuilder::new()
        .with_openai_provider("alpha", &provider_a.base_url())
        .with_openai_provider("beta", &provider_b.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    // Allow time for background model discovery
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    let resp = server.client().get(server.url("/v1/models")).send().await.unwrap();

    assert_eq!(resp.status(), 200);

    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["object"], "list");

    let models = json["data"].as_array().unwrap();
    // Should have models from both providers (prefixed with provider name)
    assert!(
        models.len() >= 2,
        "expected at least 2 models from 2 providers, got {}",
        models.len()
    );
}

// -- Unknown routes --

#[tokio::test]
async fn unknown_endpoint_returns_404() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let resp = server.client().get(server.url("/v1/nonexistent")).send().await.unwrap();

    assert_eq!(resp.status(), 404);
}

// -- Non-streaming chat with tool calls --

#[tokio::test]
async fn chat_with_tools_returns_tool_calls() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let body = serde_json::json!({
        "model": "mock-model-1",
        "messages": [{"role": "user", "content": "What is the weather?"}],
        "tools": [{
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get current weather",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "location": {"type": "string"}
                    }
                }
            }
        }]
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
    let choice = &json["choices"][0];
    assert_eq!(choice["finish_reason"], "tool_calls");

    let tool_calls = choice["message"]["tool_calls"].as_array().unwrap();
    assert!(!tool_calls.is_empty());
    assert_eq!(tool_calls[0]["function"]["name"], "get_weather");
}

// -- Custom response content --

#[tokio::test]
async fn custom_response_content() {
    let mock = MockLlm::start_with_response("custom test response").await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let body = serde_json::json!({
        "model": "mock-model-1",
        "messages": [{"role": "user", "content": "Hello"}]
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
    assert_eq!(json["choices"][0]["message"]["content"], "custom test response");
}
