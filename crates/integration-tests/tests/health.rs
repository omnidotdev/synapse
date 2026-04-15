mod harness;

use harness::config::ConfigBuilder;
use harness::mock_llm::MockLlm;
use harness::server::TestServer;

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .build();

    let server = TestServer::start(config).await.unwrap();

    let resp = server.client().get(server.url("/health")).send().await.unwrap();

    assert_eq!(resp.status(), 200);

    let body = resp.text().await.unwrap();
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn health_endpoint_disabled() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .without_health()
        .build();

    let server = TestServer::start(config).await.unwrap();

    let resp = server.client().get(server.url("/health")).send().await.unwrap();

    assert_eq!(resp.status(), 404);
}
