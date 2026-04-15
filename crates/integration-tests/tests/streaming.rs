mod harness;

use harness::config::ConfigBuilder;
use harness::mock_llm::MockLlm;
use harness::server::TestServer;

fn streaming_body(model: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "Hello"}],
        "stream": true,
        "stream_options": {"include_usage": true}
    })
}

fn streaming_body_with_tools(model: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "What is the weather?"}],
        "stream": true,
        "stream_options": {"include_usage": true},
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
    })
}

/// Parse SSE event lines from raw response text
fn parse_sse_data(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.starts_with("data: "))
        .map(|line| line.trim_start_matches("data: ").to_owned())
        .collect()
}

#[tokio::test]
async fn streaming_returns_sse_content_type() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let resp = server
        .client()
        .post(server.url("/v1/chat/completions"))
        .json(&streaming_body("mock-model-1"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        content_type.contains("text/event-stream"),
        "expected text/event-stream, got {content_type}"
    );
}

#[tokio::test]
async fn streaming_chunks_contain_content() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let resp = server
        .client()
        .post(server.url("/v1/chat/completions"))
        .json(&streaming_body("mock-model-1"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    let text = resp.text().await.unwrap();
    let events = parse_sse_data(&text);

    // Should have multiple events
    assert!(
        events.len() >= 3,
        "expected at least 3 SSE events, got {}",
        events.len()
    );

    // Collect content from all chunk events
    let mut full_content = String::new();
    for event_data in &events {
        if event_data == "[DONE]" {
            continue;
        }
        if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(event_data)
            && chunk["object"] == "chat.completion.chunk"
            && let Some(content) = chunk["choices"].get(0).and_then(|c| c["delta"]["content"].as_str())
        {
            full_content.push_str(content);
        }
    }

    // Content should reconstruct to the mock response
    let trimmed = full_content.trim();
    assert!(
        trimmed.contains("Hello") && trimmed.contains("mock") && trimmed.contains("LLM"),
        "expected reconstructed content to contain 'Hello from mock LLM', got '{trimmed}'"
    );
}

#[tokio::test]
async fn streaming_ends_with_done() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let resp = server
        .client()
        .post(server.url("/v1/chat/completions"))
        .json(&streaming_body("mock-model-1"))
        .send()
        .await
        .unwrap();

    let text = resp.text().await.unwrap();
    let events = parse_sse_data(&text);

    let last_event = events.last().expect("should have at least one event");
    assert_eq!(last_event, "[DONE]", "stream should end with [DONE]");
}

#[tokio::test]
async fn streaming_includes_usage() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let resp = server
        .client()
        .post(server.url("/v1/chat/completions"))
        .json(&streaming_body("mock-model-1"))
        .send()
        .await
        .unwrap();

    let text = resp.text().await.unwrap();
    let events = parse_sse_data(&text);

    // Find the usage event (should be a chunk with empty choices and usage field)
    let has_usage = events.iter().any(|data| {
        if data == "[DONE]" {
            return false;
        }
        serde_json::from_str::<serde_json::Value>(data)
            .ok()
            .and_then(|chunk| {
                let usage = chunk.get("usage")?;
                if usage.is_null() {
                    return None;
                }
                Some(
                    usage.get("prompt_tokens").is_some()
                        && usage.get("completion_tokens").is_some()
                        && usage.get("total_tokens").is_some(),
                )
            })
            .unwrap_or(false)
    });

    assert!(has_usage, "stream should include a usage event");
}

#[tokio::test]
async fn streaming_tool_call_encoding() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let resp = server
        .client()
        .post(server.url("/v1/chat/completions"))
        .json(&streaming_body_with_tools("mock-model-1"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    let text = resp.text().await.unwrap();
    let events = parse_sse_data(&text);

    // Look for tool call data in the stream
    let mut found_tool_call_id = false;
    let mut found_tool_call_name = false;
    let mut found_tool_call_args = false;
    let mut found_tool_calls_finish = false;

    for event_data in &events {
        if event_data == "[DONE]" {
            continue;
        }
        if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(event_data)
            && let Some(choices) = chunk["choices"].as_array()
        {
            for choice in choices {
                // Check for tool_calls in delta
                if let Some(tool_calls) = choice["delta"]["tool_calls"].as_array() {
                    for tc in tool_calls {
                        if tc.get("id").and_then(|v| v.as_str()).is_some() {
                            found_tool_call_id = true;
                        }
                        if let Some(func) = tc.get("function") {
                            if func.get("name").and_then(|v| v.as_str()).is_some() {
                                found_tool_call_name = true;
                            }
                            if func.get("arguments").and_then(|v| v.as_str()).is_some() {
                                found_tool_call_args = true;
                            }
                        }
                    }
                }

                // Check for tool_calls finish reason
                if choice["finish_reason"].as_str() == Some("tool_calls") {
                    found_tool_calls_finish = true;
                }
            }
        }
    }

    assert!(found_tool_call_id, "should contain tool call with ID");
    assert!(found_tool_call_name, "should contain tool call function name");
    assert!(found_tool_call_args, "should contain tool call arguments");
    assert!(found_tool_calls_finish, "should have tool_calls finish reason");
}

#[tokio::test]
async fn streaming_chunks_have_correct_object_type() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let resp = server
        .client()
        .post(server.url("/v1/chat/completions"))
        .json(&streaming_body("mock-model-1"))
        .send()
        .await
        .unwrap();

    let text = resp.text().await.unwrap();
    let events = parse_sse_data(&text);

    for event_data in &events {
        if event_data == "[DONE]" {
            continue;
        }
        let chunk: serde_json::Value = serde_json::from_str(event_data)
            .unwrap_or_else(|e| panic!("failed to parse SSE chunk: {e}\ndata: {event_data}"));
        assert_eq!(
            chunk["object"], "chat.completion.chunk",
            "streaming chunks should have object type 'chat.completion.chunk'"
        );
    }
}
