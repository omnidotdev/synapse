mod harness;

use harness::config::ConfigBuilder;
use harness::mock_llm::MockLlm;
use harness::server::TestServer;
use synapse_config::EquivalenceGroup;

fn completion_body(model: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "Hello"}]
    })
}

#[tokio::test]
async fn primary_succeeds_no_failover() {
    let primary = MockLlm::start().await.unwrap();
    let backup = MockLlm::start_with_response("backup response").await.unwrap();

    let config = ConfigBuilder::new()
        .with_openai_provider("primary", &primary.base_url())
        .with_openai_provider("backup", &backup.base_url())
        .with_failover(vec![EquivalenceGroup {
            name: "test".to_owned(),
            models: vec!["primary/mock-model-1".to_owned(), "backup/mock-model-1".to_owned()],
        }])
        .build();

    let server = TestServer::start(config).await.unwrap();

    // Use implicit model name so failover path is used (explicit "provider/model"
    // bypasses failover by design)
    let resp = server
        .client()
        .post(server.url("/v1/chat/completions"))
        .json(&completion_body("mock-model-1"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["choices"][0]["message"]["content"], "Hello from mock LLM");

    // Primary handled it, backup was not called
    assert_eq!(primary.completion_count(), 1);
    assert_eq!(backup.completion_count(), 0);
}

#[tokio::test]
async fn primary_fails_failover_to_backup() {
    // Primary fails the first request, backup should handle it
    let primary = MockLlm::start_failing(1).await.unwrap();
    let backup = MockLlm::start_with_response("backup response").await.unwrap();

    let config = ConfigBuilder::new()
        .with_openai_provider("primary", &primary.base_url())
        .with_openai_provider("backup", &backup.base_url())
        .with_failover(vec![EquivalenceGroup {
            name: "test".to_owned(),
            models: vec!["primary/mock-model-1".to_owned(), "backup/mock-model-1".to_owned()],
        }])
        .build();

    let server = TestServer::start(config).await.unwrap();

    // Use implicit model name so failover path is used (explicit "provider/model"
    // bypasses failover by design)
    let resp = server
        .client()
        .post(server.url("/v1/chat/completions"))
        .json(&completion_body("mock-model-1"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);

    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["choices"][0]["message"]["content"], "backup response");

    // Both providers received requests
    assert_eq!(primary.completion_count(), 1);
    assert_eq!(backup.completion_count(), 1);
}

#[tokio::test]
async fn non_retryable_error_no_failover() {
    // Provider not found is not retryable, should not trigger failover
    let backup = MockLlm::start().await.unwrap();

    let config = ConfigBuilder::new()
        .with_openai_provider("backup", &backup.base_url())
        .with_failover(vec![EquivalenceGroup {
            name: "test".to_owned(),
            models: vec!["nonexistent/model".to_owned(), "backup/mock-model-1".to_owned()],
        }])
        .build();

    let server = TestServer::start(config).await.unwrap();

    let resp = server
        .client()
        .post(server.url("/v1/chat/completions"))
        .json(&completion_body("nonexistent/model"))
        .send()
        .await
        .unwrap();

    // Provider not found → 404 with no failover
    assert!(resp.status().is_client_error());
    assert_eq!(backup.completion_count(), 0);
}

#[tokio::test]
async fn all_providers_fail_returns_error() {
    let primary = MockLlm::start_failing(10).await.unwrap();
    let backup = MockLlm::start_failing(10).await.unwrap();

    let config = ConfigBuilder::new()
        .with_openai_provider("primary", &primary.base_url())
        .with_openai_provider("backup", &backup.base_url())
        .with_failover(vec![EquivalenceGroup {
            name: "test".to_owned(),
            models: vec!["primary/mock-model-1".to_owned(), "backup/mock-model-1".to_owned()],
        }])
        .build();

    let server = TestServer::start(config).await.unwrap();

    // Use implicit model name so failover path is used
    let resp = server
        .client()
        .post(server.url("/v1/chat/completions"))
        .json(&completion_body("mock-model-1"))
        .send()
        .await
        .unwrap();

    // Both failed, should return an error
    assert!(resp.status().is_server_error());
}

#[tokio::test]
async fn failover_disabled_returns_primary_error() {
    let primary = MockLlm::start_failing(1).await.unwrap();
    let backup = MockLlm::start().await.unwrap();

    // No failover configured
    let config = ConfigBuilder::new()
        .with_openai_provider("primary", &primary.base_url())
        .with_openai_provider("backup", &backup.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let resp = server
        .client()
        .post(server.url("/v1/chat/completions"))
        .json(&completion_body("primary/mock-model-1"))
        .send()
        .await
        .unwrap();

    // Failover disabled, primary error returned
    assert!(resp.status().is_server_error());
    assert_eq!(backup.completion_count(), 0);
}
