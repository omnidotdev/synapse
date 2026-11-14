//! Upstream key selection through `LlmState`: a BYOK request runs on the
//! caller's own provider key, a managed request on the managed key, and a
//! request without a billing identity on the provider's configured key, for
//! every provider protocol and across failover to another provider

use std::collections::HashMap;

use futures_util::StreamExt;
use secrecy::SecretString;
use synapse_config::LlmConfig;
use synapse_core::{BillingIdentity, BillingMode, RequestContext};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::error::LlmError;
use crate::state::LlmState;
use crate::types::CompletionRequest;

/// A provider protocol under test: its config name, the header it carries the
/// key in (with any scheme prefix), and an upstream success body
struct Upstream {
    name: &'static str,
    kind: &'static str,
    base_path: &'static str,
    model: &'static str,
    header: &'static str,
    scheme: &'static str,
}

const OPENAI: Upstream = Upstream {
    name: "openai",
    kind: "openai",
    base_path: "/v1",
    model: "gpt-test",
    header: "authorization",
    scheme: "Bearer ",
};

const ANTHROPIC: Upstream = Upstream {
    name: "anthropic",
    kind: "anthropic",
    base_path: "/v1",
    model: "claude-test",
    header: "x-api-key",
    scheme: "",
};

const GOOGLE: Upstream = Upstream {
    name: "google",
    kind: "google",
    base_path: "/v1beta",
    model: "gemini-test",
    header: "x-goog-api-key",
    scheme: "",
};

const UPSTREAMS: [Upstream; 3] = [OPENAI, ANTHROPIC, GOOGLE];

fn success_body(upstream: &Upstream) -> serde_json::Value {
    match upstream.kind {
        "openai" => serde_json::json!({
            "id": "c1",
            "object": "chat.completion",
            "created": 1,
            "model": upstream.model,
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        }),
        "anthropic" => serde_json::json!({
            "id": "m1",
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "text", "text": "ok" }],
            "model": upstream.model,
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 1, "output_tokens": 1 }
        }),
        _ => serde_json::json!({
            "candidates": [{
                "content": { "role": "model", "parts": [{ "text": "ok" }] },
                "finishReason": "STOP"
            }],
            "usageMetadata": { "promptTokenCount": 1, "candidatesTokenCount": 1, "totalTokenCount": 2 }
        }),
    }
}

async fn mount_success(server: &MockServer, upstream: &Upstream) {
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_body(upstream)))
        .mount(server)
        .await;
}

/// Each upstream on its own mock server, every model priced so managed
/// requests pass the pre-request cost estimate
async fn state_with(
    servers: &[(&Upstream, &MockServer)],
    failover_group: Option<&[&str]>,
    managed_keys: &[(&str, &str)],
) -> LlmState {
    let mut providers = serde_json::Map::new();
    let mut models = Vec::new();
    for (upstream, server) in servers {
        providers.insert(
            upstream.name.to_owned(),
            serde_json::json!({
                "type": upstream.kind,
                "api_key": format!("configured-{}", upstream.name),
                "base_url": format!("{}{}", server.uri(), upstream.base_path),
                // A bare alias resolves without the explicit "provider/" form,
                // so failover applies to it
                "models": { "overrides": { upstream.model: { "alias": format!("{}-alias", upstream.name) } } },
            }),
        );
        models.push(serde_json::json!({
            "provider": upstream.name,
            "model": upstream.model,
            "context_window": 200_000,
            "input_per_mtok": 1.0,
            "output_per_mtok": 1.0,
            "quality": 0.9,
        }));
    }

    let mut config = serde_json::json!({
        "providers": providers,
        "routing": { "models": models },
    });
    if let Some(group) = failover_group {
        config["failover"] = serde_json::json!({
            "enabled": true,
            "max_attempts": 2,
            "equivalence_groups": [{ "name": "group", "models": group }],
        });
    }
    let config: LlmConfig = serde_json::from_value(config).unwrap();

    let mut state = LlmState::from_config(config).await.unwrap();
    state.set_managed_keys(
        managed_keys
            .iter()
            .map(|(name, key)| ((*name).to_owned(), SecretString::from(*key)))
            .collect::<HashMap<_, _>>(),
    );
    state
}

fn request(model: &str, stream: bool) -> CompletionRequest {
    serde_json::from_value(serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": "hi" }],
        "params": { "max_tokens": 16 },
        "stream": stream,
    }))
    .unwrap()
}

fn identity(mode: BillingMode) -> BillingIdentity {
    BillingIdentity {
        app_id: "synapse".to_owned(),
        entity_type: "user".to_owned(),
        entity_id: "usr_1".to_owned(),
        mode,
        plan: Some("pro".to_owned()),
    }
}

fn byok_context(keys: &[(&str, &str)]) -> RequestContext {
    let mut ctx = RequestContext::empty();
    ctx.billing_identity = Some(identity(BillingMode::Byok));
    ctx.provider_keys = keys
        .iter()
        .map(|(name, key)| ((*name).to_owned(), SecretString::from(*key)))
        .collect();
    ctx
}

fn managed_context() -> RequestContext {
    let mut ctx = RequestContext::empty();
    ctx.billing_identity = Some(identity(BillingMode::Managed));
    ctx
}

/// The key each request to `server` carried upstream, scheme stripped
async fn keys_sent(server: &MockServer, upstream: &Upstream) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method == wiremock::http::Method::POST)
        .map(|r| {
            let value = r
                .headers
                .get(upstream.header)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            value.strip_prefix(upstream.scheme).unwrap_or(&value).to_owned()
        })
        .collect()
}

#[tokio::test]
async fn byok_request_runs_on_the_callers_key_for_every_provider_type() {
    for upstream in &UPSTREAMS {
        let server = MockServer::start().await;
        mount_success(&server, upstream).await;
        let state = state_with(&[(upstream, &server)], None, &[(upstream.name, "managed-key")]).await;

        let caller_key = format!("byok-{}", upstream.name);
        state
            .complete(
                request(&format!("{}/{}", upstream.name, upstream.model), false),
                byok_context(&[(upstream.name, &caller_key)]),
            )
            .await
            .unwrap();

        assert_eq!(
            keys_sent(&server, upstream).await,
            vec![caller_key],
            "{}",
            upstream.name
        );
    }
}

#[tokio::test]
async fn byok_stream_runs_on_the_callers_key_for_every_provider_type() {
    for upstream in &UPSTREAMS {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream"))
            .mount(&server)
            .await;
        let state = state_with(&[(upstream, &server)], None, &[]).await;

        let caller_key = format!("byok-{}", upstream.name);
        let (_, mut stream) = state
            .complete_stream(
                request(&format!("{}/{}", upstream.name, upstream.model), true),
                byok_context(&[(upstream.name, &caller_key)]),
            )
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        assert_eq!(
            keys_sent(&server, upstream).await,
            vec![caller_key],
            "{}",
            upstream.name
        );
    }
}

#[tokio::test]
async fn byok_request_without_a_key_for_the_provider_is_rejected_before_any_upstream_call() {
    for upstream in &UPSTREAMS {
        let server = MockServer::start().await;
        mount_success(&server, upstream).await;
        let state = state_with(&[(upstream, &server)], None, &[]).await;

        let err = state
            .complete(
                request(&format!("{}/{}", upstream.name, upstream.model), false),
                byok_context(&[("some-other-provider", "byok-other")]),
            )
            .await
            .unwrap_err();

        assert!(matches!(err, LlmError::InvalidRequest(_)), "{}: {err:?}", upstream.name);
        assert!(keys_sent(&server, upstream).await.is_empty(), "{}", upstream.name);
    }
}

#[tokio::test]
async fn managed_request_runs_on_the_managed_key_for_every_provider_type() {
    for upstream in &UPSTREAMS {
        let server = MockServer::start().await;
        mount_success(&server, upstream).await;
        let managed_key = format!("managed-{}", upstream.name);
        let state = state_with(&[(upstream, &server)], None, &[(upstream.name, &managed_key)]).await;

        let mut ctx = managed_context();
        // A stray BYOK key on a managed request is never used
        ctx.provider_keys
            .insert(upstream.name.to_owned(), SecretString::from("byok-ignored"));
        state
            .complete(request(&format!("{}/{}", upstream.name, upstream.model), false), ctx)
            .await
            .unwrap();

        assert_eq!(
            keys_sent(&server, upstream).await,
            vec![managed_key],
            "{}",
            upstream.name
        );
    }
}

#[tokio::test]
async fn managed_request_without_a_managed_key_runs_on_the_configured_key() {
    for upstream in &UPSTREAMS {
        let server = MockServer::start().await;
        mount_success(&server, upstream).await;
        let state = state_with(&[(upstream, &server)], None, &[]).await;

        state
            .complete(
                request(&format!("{}/{}", upstream.name, upstream.model), false),
                managed_context(),
            )
            .await
            .unwrap();

        assert_eq!(
            keys_sent(&server, upstream).await,
            vec![format!("configured-{}", upstream.name)],
            "{}",
            upstream.name
        );
    }
}

#[tokio::test]
async fn request_without_a_billing_identity_runs_on_the_configured_key() {
    for upstream in &UPSTREAMS {
        let server = MockServer::start().await;
        mount_success(&server, upstream).await;
        let state = state_with(&[(upstream, &server)], None, &[(upstream.name, "managed-key")]).await;

        state
            .complete(
                request(&format!("{}/{}", upstream.name, upstream.model), false),
                RequestContext::empty(),
            )
            .await
            .unwrap();

        assert_eq!(
            keys_sent(&server, upstream).await,
            vec![format!("configured-{}", upstream.name)],
            "{}",
            upstream.name
        );
    }
}

/// Primary (`OpenAI`) fails with a retryable error, so the request fails over
/// to the Anthropic equivalent
async fn failover_pair() -> (MockServer, MockServer) {
    let (primary, alternate) = (MockServer::start().await, MockServer::start().await);
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&primary)
        .await;
    mount_success(&alternate, &ANTHROPIC).await;
    (primary, alternate)
}

const FAILOVER_GROUP: &[&str] = &["openai/gpt-test", "anthropic/claude-test"];

#[tokio::test]
async fn byok_failover_sends_the_callers_key_for_the_alternate_provider() {
    let (primary, alternate) = failover_pair().await;
    let state = state_with(
        &[(&OPENAI, &primary), (&ANTHROPIC, &alternate)],
        Some(FAILOVER_GROUP),
        &[],
    )
    .await;

    state
        .complete(
            request("openai-alias", false),
            byok_context(&[("openai", "byok-openai"), ("anthropic", "byok-anthropic")]),
        )
        .await
        .unwrap();

    assert_eq!(keys_sent(&primary, &OPENAI).await, vec!["byok-openai"]);
    assert_eq!(keys_sent(&alternate, &ANTHROPIC).await, vec!["byok-anthropic"]);
}

#[tokio::test]
async fn byok_failover_skips_an_alternate_the_caller_has_no_key_for() {
    let (primary, alternate) = failover_pair().await;
    let state = state_with(
        &[(&OPENAI, &primary), (&ANTHROPIC, &alternate)],
        Some(FAILOVER_GROUP),
        &[("anthropic", "managed-anthropic")],
    )
    .await;

    let err = state
        .complete(
            request("openai-alias", false),
            byok_context(&[("openai", "byok-openai")]),
        )
        .await
        .unwrap_err();

    // The primary's upstream failure surfaces, and neither the caller's
    // OpenAI key nor any Omni key ever reaches the alternate
    assert!(matches!(err, LlmError::Upstream(_)), "{err:?}");
    assert!(keys_sent(&alternate, &ANTHROPIC).await.is_empty());
}

#[tokio::test]
async fn managed_failover_sends_the_managed_key_for_the_alternate_provider() {
    let (primary, alternate) = failover_pair().await;
    let state = state_with(
        &[(&OPENAI, &primary), (&ANTHROPIC, &alternate)],
        Some(FAILOVER_GROUP),
        &[("openai", "managed-openai"), ("anthropic", "managed-anthropic")],
    )
    .await;

    state
        .complete(request("openai-alias", false), managed_context())
        .await
        .unwrap();

    assert_eq!(keys_sent(&primary, &OPENAI).await, vec!["managed-openai"]);
    assert_eq!(keys_sent(&alternate, &ANTHROPIC).await, vec!["managed-anthropic"]);
}

#[tokio::test]
async fn managed_failover_to_a_provider_without_a_managed_key_uses_its_configured_key() {
    let (primary, alternate) = failover_pair().await;
    let state = state_with(
        &[(&OPENAI, &primary), (&ANTHROPIC, &alternate)],
        Some(FAILOVER_GROUP),
        &[("openai", "managed-openai")],
    )
    .await;

    state
        .complete(request("openai-alias", false), managed_context())
        .await
        .unwrap();

    assert_eq!(keys_sent(&alternate, &ANTHROPIC).await, vec!["configured-anthropic"]);
}

#[tokio::test]
async fn byok_stream_failover_sends_the_callers_key_for_the_alternate_provider() {
    let (primary, alternate) = (MockServer::start().await, MockServer::start().await);
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&primary)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream"))
        .mount(&alternate)
        .await;
    let state = state_with(
        &[(&OPENAI, &primary), (&ANTHROPIC, &alternate)],
        Some(FAILOVER_GROUP),
        &[],
    )
    .await;

    let (_, mut stream) = state
        .complete_stream(
            request("openai-alias", true),
            byok_context(&[("openai", "byok-openai"), ("anthropic", "byok-anthropic")]),
        )
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    assert_eq!(keys_sent(&alternate, &ANTHROPIC).await, vec!["byok-anthropic"]);
}

#[tokio::test]
async fn byok_request_to_bedrock_is_rejected_since_it_cannot_carry_a_caller_key() {
    let config: LlmConfig = serde_json::from_value(serde_json::json!({
        "providers": {
            "bedrock": {
                "type": { "bedrock": {
                    "region": "us-east-1",
                    "access_key_id": "omni-access",
                    "secret_access_key": "omni-secret",
                } },
            },
        },
    }))
    .unwrap();
    let state = LlmState::from_config(config).await.unwrap();

    let err = state
        .complete(
            request("bedrock/anthropic.claude-test", false),
            byok_context(&[("bedrock", "byok-bedrock")]),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, LlmError::InvalidRequest(_)), "{err:?}");
}
