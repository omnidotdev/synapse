mod harness;

use harness::config::ConfigBuilder;
use harness::mock_llm::MockLlm;
use harness::server::TestServer;
use synapse_config::{AnyOrArray, CorsConfig, CsrfConfig, RateLimitConfig, RequestRateLimit};

// -- CORS tests --

#[tokio::test]
async fn cors_allows_configured_origin() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .with_cors(CorsConfig {
            origins: AnyOrArray::List(vec!["http://example.com".to_owned()]),
            methods: AnyOrArray::Any,
            headers: AnyOrArray::Any,
            expose_headers: Vec::new(),
            credentials: false,
            max_age: None,
            private_network: false,
        })
        .build();

    let server = TestServer::start(config).await.unwrap();

    let resp = server
        .client()
        .get(server.url("/health"))
        .header("Origin", "http://example.com")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("http://example.com")
    );
}

#[tokio::test]
async fn cors_wildcard_allows_any_origin() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .with_cors(CorsConfig {
            origins: AnyOrArray::Any,
            methods: AnyOrArray::Any,
            headers: AnyOrArray::Any,
            expose_headers: Vec::new(),
            credentials: false,
            max_age: None,
            private_network: false,
        })
        .build();

    let server = TestServer::start(config).await.unwrap();

    let resp = server
        .client()
        .get(server.url("/health"))
        .header("Origin", "http://anywhere.example")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert!(resp.headers().get("access-control-allow-origin").is_some());
}

// -- CSRF tests --

#[tokio::test]
async fn csrf_blocks_post_without_header() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .with_csrf(CsrfConfig {
            enabled: true,
            header_name: "X-CSRF-Token".to_owned(),
        })
        .build();

    let server = TestServer::start(config).await.unwrap();

    let body = serde_json::json!({
        "model": "mock-model-1",
        "messages": [{"role": "user", "content": "test"}]
    });

    let resp = server
        .client()
        .post(server.url("/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn csrf_allows_post_with_header() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .with_csrf(CsrfConfig {
            enabled: true,
            header_name: "X-CSRF-Token".to_owned(),
        })
        .build();

    let server = TestServer::start(config).await.unwrap();

    let body = serde_json::json!({
        "model": "mock-model-1",
        "messages": [{"role": "user", "content": "test"}]
    });

    let resp = server
        .client()
        .post(server.url("/v1/chat/completions"))
        .header("X-CSRF-Token", "1")
        .json(&body)
        .send()
        .await
        .unwrap();

    // Should pass CSRF check (may still error on model resolution, but not 403)
    assert_ne!(resp.status(), 403);
}

#[tokio::test]
async fn csrf_allows_get_without_header() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .with_csrf(CsrfConfig {
            enabled: true,
            header_name: "X-CSRF-Token".to_owned(),
        })
        .build();

    let server = TestServer::start(config).await.unwrap();

    let resp = server.client().get(server.url("/health")).send().await.unwrap();

    assert_eq!(resp.status(), 200);
}

// -- Rate limiting tests --

#[tokio::test]
async fn rate_limit_returns_429_when_exceeded() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_openai_provider("mock", &mock.base_url())
        .with_rate_limit(RateLimitConfig {
            storage: Default::default(),
            global: Some(RequestRateLimit {
                requests: 2,
                window: "1m".to_owned(),
            }),
            per_ip: None,
            tokens: None,
        })
        .build();

    let server = TestServer::start(config).await.unwrap();

    // First two requests should succeed
    for _ in 0..2 {
        let resp = server.client().get(server.url("/health")).send().await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    // Third request should be rate limited
    let resp = server.client().get(server.url("/health")).send().await.unwrap();

    assert_eq!(resp.status(), 429);
    assert!(resp.headers().get("retry-after").is_some());
}
