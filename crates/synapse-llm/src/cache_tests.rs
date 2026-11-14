//! Response cache through `LlmState`: entries are scoped to the caller's
//! tenant and billing mode, a hit passes the same spend check a live request
//! does and is billed like one, and nothing is cached unless the caller opts in

use std::time::Duration;

use secrecy::SecretString;
use synapse_cache::ResponseCache;
use synapse_config::FailMode;
use synapse_core::{BillingMode, RequestContext};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::error::LlmError;
use crate::spend_tests::{
    ACTUAL_CREDITS, ACTUAL_USD, BodyApprox, ESTIMATED_CREDITS, QueryApprox, context, forbid_wallet, llm_state,
    mount_completion, mount_usage_meters, request,
};
use crate::state::LlmState;

async fn cached_state(provider: &MockServer, aether: &MockServer, cache: &ResponseCache) -> LlmState {
    let mut state = llm_state(provider, aether, FailMode::Degraded).await;
    state.set_response_cache(cache.clone());
    state
}

fn cache() -> ResponseCache {
    ResponseCache::in_memory(Duration::from_secs(3600))
}

/// Mark a request as opted into the response cache
fn opted_in(mut ctx: RequestContext) -> RequestContext {
    ctx.parts
        .headers
        .insert(synapse_cache::OPT_IN_HEADER, http::HeaderValue::from_static("true"));
    ctx
}

fn byok(mut ctx: RequestContext) -> RequestContext {
    if let Some(identity) = ctx.billing_identity.as_mut() {
        identity.mode = BillingMode::Byok;
    }
    ctx.provider_keys
        .insert("mock".to_owned(), SecretString::from("byok-provider-key"));
    ctx
}

/// Credit wallet that covers any check and accepts any deduct
async fn mount_funded_wallet(aether: &MockServer) {
    Mock::given(method("GET"))
        .and(path_regex(r"^/credits/facet/user/[a-z0-9_]+/check$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sufficient": true, "balance": 50.0
        })))
        .mount(aether)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/credits/facet/user/[a-z0-9_]+/deduct$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "balance": 46.0
        })))
        .mount(aether)
        .await;
}

async fn requests_to(server: &MockServer, url_path: &str) -> Vec<wiremock::Request> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.url.path() == url_path)
        .collect()
}

/// Wait for `count` fire-and-forget requests to reach the mock
async fn wait_for_requests(server: &MockServer, url_path: &str, count: usize) {
    for _ in 0..100 {
        if requests_to(server, url_path).await.len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("fewer than {count} requests to {url_path}");
}

#[tokio::test]
async fn two_tenants_with_identical_requests_never_share_an_entry() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    // Tenant A live, tenant B live, tenant A again from the cache
    mount_completion(&provider, 2).await;
    mount_funded_wallet(&aether).await;
    mount_usage_meters(&aether).await;

    let state = cached_state(&provider, &aether, &cache()).await;

    for entity_id in ["usr_a", "usr_b", "usr_a"] {
        state
            .complete(request(false), opted_in(context("facet", "user", entity_id)))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn the_same_entity_in_another_product_never_shares_an_entry() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 2).await;
    mount_funded_wallet(&aether).await;
    mount_usage_meters(&aether).await;
    Mock::given(path_regex(r"^/usage/myfi/organization/org_1/ai_credits/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "allowed": true, "current": 0, "limit": 100_000, "eventId": "e"
        })))
        .mount(&aether)
        .await;

    let state = cached_state(&provider, &aether, &cache()).await;

    state
        .complete(request(false), opted_in(context("facet", "user", "org_1")))
        .await
        .unwrap();
    state
        .complete(request(false), opted_in(context("myfi", "organization", "org_1")))
        .await
        .unwrap();
}

#[tokio::test]
async fn byok_and_managed_never_share_an_entry() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    // One live call per billing mode, the repeats are cache hits
    mount_completion(&provider, 2).await;
    mount_funded_wallet(&aether).await;
    mount_usage_meters(&aether).await;

    let state = cached_state(&provider, &aether, &cache()).await;
    let managed = || opted_in(context("facet", "user", "usr_1"));
    let byok_ctx = || byok(opted_in(context("facet", "user", "usr_1")));

    state.complete(request(false), managed()).await.unwrap();
    state.complete(request(false), byok_ctx()).await.unwrap();
    state.complete(request(false), managed()).await.unwrap();
    state.complete(request(false), byok_ctx()).await.unwrap();
}

#[tokio::test]
async fn a_managed_cache_hit_runs_the_spend_check_and_is_refused_with_402() {
    let (provider, funded, broke) = (
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    );
    mount_completion(&provider, 1).await;
    mount_funded_wallet(&funded).await;
    mount_usage_meters(&funded).await;
    Mock::given(method("GET"))
        .and(path("/credits/facet/user/usr_1/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sufficient": false, "balance": 0.0
        })))
        .expect(1)
        .named("a cache hit is checked like a live request")
        .mount(&broke)
        .await;
    Mock::given(path_regex(r"/(deduct|record)$"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .named("a refused cache hit is never served or billed")
        .mount(&broke)
        .await;

    // Both gateways share one cache: the entry exists when the caller runs out
    let shared = cache();
    let state = cached_state(&provider, &funded, &shared).await;
    state
        .complete(request(false), opted_in(context("facet", "user", "usr_1")))
        .await
        .unwrap();

    let state = cached_state(&provider, &broke, &shared).await;
    let err = state
        .complete(request(false), opted_in(context("facet", "user", "usr_1")))
        .await
        .unwrap_err();

    assert!(matches!(err, LlmError::InsufficientCredits { .. }));
    assert_eq!(
        synapse_core::HttpError::status_code(&err),
        http::StatusCode::PAYMENT_REQUIRED
    );
}

#[tokio::test]
async fn a_managed_cache_hit_is_metered_and_deducted_like_a_served_request() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 1).await;
    mount_usage_meters(&aether).await;
    Mock::given(method("GET"))
        .and(path("/credits/facet/user/usr_1/check"))
        .and(QueryApprox("amount", ESTIMATED_CREDITS / 100.0))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sufficient": true, "balance": 50.0
        })))
        .expect(2)
        .named("live request and cache hit are both checked")
        .mount(&aether)
        .await;
    Mock::given(method("POST"))
        .and(path("/credits/facet/user/usr_1/deduct"))
        .and(BodyApprox("amount", ACTUAL_USD))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "balance": 46.0
        })))
        .expect(2)
        .named("live request and cache hit are both deducted at the cached usage")
        .mount(&aether)
        .await;

    let state = cached_state(&provider, &aether, &cache()).await;

    let live = state
        .complete(request(false), opted_in(context("facet", "user", "usr_1")))
        .await
        .unwrap();
    let hit = state
        .complete(request(false), opted_in(context("facet", "user", "usr_1")))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&live).unwrap(),
        serde_json::to_value(&hit).unwrap()
    );

    // Each charge is its own event with a fresh idempotency key
    let deducts = requests_to(&aether, "/credits/facet/user/usr_1/deduct").await;
    let keys: Vec<_> = deducts
        .iter()
        .map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).unwrap()["idempotencyKey"].clone())
        .collect();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);

    // Usage events record for the hit as for the live request
    wait_for_requests(&aether, "/usage/facet/user/usr_1/requests/record", 2).await;
    wait_for_requests(&aether, "/usage/facet/user/usr_1/input_tokens/record", 2).await;
}

#[tokio::test]
async fn an_allowance_cache_hit_is_checked_and_charged_to_the_allowance_meter() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 1).await;
    mount_usage_meters(&aether).await;
    forbid_wallet(&aether).await;
    Mock::given(method("GET"))
        .and(path("/usage/myfi/organization/org_1/ai_credits/check"))
        .and(QueryApprox("additionalUsage", ESTIMATED_CREDITS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "allowed": true, "current": 0, "limit": 100_000
        })))
        .expect(2)
        .named("live request and cache hit are both checked against the allowance")
        .mount(&aether)
        .await;
    Mock::given(method("POST"))
        .and(path("/usage/myfi/organization/org_1/ai_credits/record"))
        .and(BodyApprox("delta", ACTUAL_CREDITS))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "eventId": "e" })))
        .expect(2)
        .named("live request and cache hit are both charged to the allowance")
        .mount(&aether)
        .await;

    let state = cached_state(&provider, &aether, &cache()).await;

    for _ in 0..2 {
        state
            .complete(request(false), opted_in(context("myfi", "organization", "org_1")))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn an_exhausted_allowance_refuses_a_cache_hit() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 1).await;
    mount_usage_meters(&aether).await;
    forbid_wallet(&aether).await;
    Mock::given(method("POST"))
        .and(path("/usage/myfi/organization/org_1/ai_credits/record"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "eventId": "e" })))
        .expect(1)
        .named("only the live request is charged")
        .mount(&aether)
        .await;
    // Allowed for the live request, exhausted by the time of the repeat
    Mock::given(method("GET"))
        .and(path("/usage/myfi/organization/org_1/ai_credits/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "allowed": true, "current": 0, "limit": 1000
        })))
        .up_to_n_times(1)
        .mount(&aether)
        .await;
    Mock::given(method("GET"))
        .and(path("/usage/myfi/organization/org_1/ai_credits/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "allowed": false, "current": 1000, "limit": 1000
        })))
        .mount(&aether)
        .await;

    let state = cached_state(&provider, &aether, &cache()).await;

    state
        .complete(request(false), opted_in(context("myfi", "organization", "org_1")))
        .await
        .unwrap();
    let err = state
        .complete(request(false), opted_in(context("myfi", "organization", "org_1")))
        .await
        .unwrap_err();

    assert!(matches!(err, LlmError::AllowanceExhausted));
}

#[tokio::test]
async fn regenerate_without_opt_in_is_never_cached() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    // No temperature and no opt-in: every regenerate reaches the model
    mount_completion(&provider, 2).await;
    mount_funded_wallet(&aether).await;
    mount_usage_meters(&aether).await;

    let state = cached_state(&provider, &aether, &cache()).await;
    let req = request(false);
    assert!(req.params.temperature.is_none());

    for _ in 0..2 {
        state
            .complete(req.clone(), context("facet", "user", "usr_1"))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn an_anonymous_request_is_never_cached() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 2).await;

    let state = cached_state(&provider, &aether, &cache()).await;

    for _ in 0..2 {
        state
            .complete(request(false), opted_in(RequestContext::empty()))
            .await
            .unwrap();
    }
}
