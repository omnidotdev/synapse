//! Integration tests for synapse-client embedded mode

mod harness;

use harness::config::ConfigBuilder;
use harness::mock_llm::MockLlm;
use synapse_client::{ChatRequest, Message, SynapseClient};

#[tokio::test]
async fn embedded_chat_completion() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("test", &mock.base_url())
        .build();

    let client = SynapseClient::embedded(config).await.unwrap();

    let request = ChatRequest {
        model: "test/mock-model-1".to_owned(),
        messages: vec![Message::user("Hello")],
        stream: false,
        temperature: None,
        top_p: None,
        max_tokens: None,
        stop: None,
        tools: None,
        tool_choice: None,
    };

    let response = client.chat_completion(&request).await.unwrap();
    assert_eq!(response.choices.len(), 1);
    assert_eq!(
        response.choices[0].message.content.as_deref(),
        Some("Hello from mock LLM")
    );
    assert!(response.usage.is_some());
    assert_eq!(mock.completion_count(), 1);
}

#[tokio::test]
async fn embedded_list_models() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("test", &mock.base_url())
        .build();

    let client = SynapseClient::embedded(config).await.unwrap();

    // Allow time for background model discovery to complete
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    let models = client.list_models().await.unwrap();
    assert!(!models.is_empty());
}

#[tokio::test]
async fn embedded_stt_returns_error() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("test", &mock.base_url())
        .build();

    let client = SynapseClient::embedded(config).await.unwrap();
    let result = client.transcribe(bytes::Bytes::new(), "test.wav", "whisper-1").await;
    assert!(result.is_err());
}
