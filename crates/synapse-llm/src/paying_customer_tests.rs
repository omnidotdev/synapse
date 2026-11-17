//! Managed AI billed to the pooled credit wallet is served only to a paying
//! customer: a free tier that never bought credits is refused with 402
//! `payment_required` before any provider call, whatever its balance of free
//! grants, while allowance products and BYOK are not gated

use std::collections::HashMap;

use axum::body::Body;
use secrecy::SecretString;
use synapse_billing::{AetherClient, PayingCustomerGate, SpendPolicy};
use synapse_config::{FailMode, SpendFailModes};
use synapse_core::BillingMode;
use tower::ServiceExt;
use url::Url;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::error::LlmError;
use crate::handler::llm_router;
use crate::spend_tests::{MODEL, context, llm_state_with, mount_completion, mount_usage_meters, request};
use crate::state::LlmState;

const PROD_FAIL_MODES: SpendFailModes = SpendFailModes {
    default: FailMode::Degraded,
    wallet: FailMode::Closed,
};

/// State whose spend checks run behind the paying-customer gate
async fn gated_state(provider: &MockServer, aether: &MockServer, fail_modes: SpendFailModes) -> LlmState {
    let mut state = llm_state_with(provider, aether, fail_modes.clone()).await;
    let client = AetherClient::new(
        Url::parse(&format!("{}/", aether.uri())).unwrap(),
        SecretString::from("aether-key".to_owned()),
    )
    .unwrap()
    .with_spend_policy(SpendPolicy::new(HashMap::from([(
        "myfi".to_owned(),
        "ai_credits".to_owned(),
    )])))
    .with_paying_customer_gate(PayingCustomerGate::new());
    state.set_billing_client(client, fail_modes);
    state
}

async fn mount_tier(aether: &MockServer, tier: &str) {
    Mock::given(method("GET"))
        .and(path("/entitlements/facet/user/usr_1/tier/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "hasEntitlement": true, "value": tier, "version": 1
        })))
        .mount(aether)
        .await;
}

async fn mount_funding(aether: &MockServer, is_paying: bool) {
    Mock::given(method("GET"))
        .and(path("/credits/facet/user/usr_1/funding"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "billingAccountId": "ba_1",
            "paidCreditsUsd": if is_paying { 10.0 } else { 0.0 },
            "isPayingCustomer": is_paying
        })))
        .mount(aether)
        .await;
}

/// A wallet stocked with free grants: enough balance, never charged
async fn mount_free_grant_wallet(aether: &MockServer, checks: u64) {
    Mock::given(method("GET"))
        .and(path("/credits/facet/user/usr_1/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sufficient": true, "balance": 25.0
        })))
        .expect(checks)
        .mount(aether)
        .await;
    Mock::given(method("POST"))
        .and(path("/credits/facet/user/usr_1/deduct"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true, "balance": 21.0
        })))
        .expect(checks)
        .mount(aether)
        .await;
}

/// A free tier that never bought credits, with free grants in its wallet
async fn mount_free_account(aether: &MockServer) {
    mount_tier(aether, "free").await;
    mount_funding(aether, false).await;
    mount_free_grant_wallet(aether, 0).await;
}

#[tokio::test]
async fn a_free_account_is_refused_with_402_before_the_provider_is_called() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 0).await;
    mount_free_account(&aether).await;

    let state = gated_state(&provider, &aether, PROD_FAIL_MODES).await;

    let err = state
        .complete(request(false), context("facet", "user", "usr_1"))
        .await
        .unwrap_err();

    assert!(matches!(err, LlmError::PaymentRequired));
    assert_eq!(
        synapse_core::HttpError::status_code(&err),
        http::StatusCode::PAYMENT_REQUIRED
    );
    assert_eq!(synapse_core::HttpError::error_type(&err), "payment_required");
    assert_eq!(
        synapse_core::HttpError::client_message(&err),
        "AI generation uses Omni credits. Buy credits or upgrade your plan to start."
    );
    assert!(!err.is_retryable(), "a 402 must never fail over to a backup model");
}

#[tokio::test]
async fn a_free_account_is_refused_before_streaming() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 0).await;
    mount_free_account(&aether).await;

    let state = gated_state(&provider, &aether, PROD_FAIL_MODES).await;

    let result = state
        .complete_stream(request(true), context("facet", "user", "usr_1"))
        .await;

    assert!(matches!(result, Err(LlmError::PaymentRequired)));
}

#[tokio::test]
async fn a_free_account_that_bought_credits_is_served() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 1).await;
    mount_tier(&aether, "free").await;
    mount_funding(&aether, true).await;
    mount_free_grant_wallet(&aether, 1).await;
    mount_usage_meters(&aether).await;

    let state = gated_state(&provider, &aether, PROD_FAIL_MODES).await;

    state
        .complete(request(false), context("facet", "user", "usr_1"))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_paid_tier_is_served_without_a_funding_check() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 1).await;
    mount_tier(&aether, "pro").await;
    Mock::given(path_regex(r"/funding$"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&aether)
        .await;
    mount_free_grant_wallet(&aether, 1).await;
    mount_usage_meters(&aether).await;

    let state = gated_state(&provider, &aether, PROD_FAIL_MODES).await;

    state
        .complete(request(false), context("facet", "user", "usr_1"))
        .await
        .unwrap();
}

#[tokio::test]
async fn an_unreachable_funding_check_follows_the_wallet_fail_mode() {
    for (wallet, served) in [(FailMode::Closed, false), (FailMode::Degraded, true)] {
        let (provider, aether) = (MockServer::start().await, MockServer::start().await);
        mount_completion(&provider, u64::from(served)).await;
        mount_tier(&aether, "free").await;
        Mock::given(method("GET"))
            .and(path("/credits/facet/user/usr_1/funding"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&aether)
            .await;
        mount_usage_meters(&aether).await;
        Mock::given(method("POST"))
            .and(path("/credits/facet/user/usr_1/deduct"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true, "balance": 0.0
            })))
            .mount(&aether)
            .await;

        let modes = SpendFailModes {
            default: FailMode::Degraded,
            wallet: wallet.clone(),
        };
        let state = gated_state(&provider, &aether, modes).await;

        let result = state.complete(request(false), context("facet", "user", "usr_1")).await;

        if served {
            assert!(result.is_ok(), "{wallet:?} must serve through the outage");
        } else {
            assert!(
                matches!(result, Err(LlmError::BillingUnavailable)),
                "{wallet:?} must refuse with 503"
            );
        }
    }
}

#[tokio::test]
async fn an_allowance_product_is_not_gated() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 1).await;
    mount_usage_meters(&aether).await;
    Mock::given(path_regex(r"^/entitlements/|^/credits/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .named("an allowance product never reads its tier for the gate or touches the wallet")
        .mount(&aether)
        .await;
    Mock::given(method("GET"))
        .and(path("/usage/myfi/organization/org_1/ai_credits/check"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "allowed": true, "current": 0, "limit": 1000
        })))
        .mount(&aether)
        .await;

    let state = gated_state(&provider, &aether, PROD_FAIL_MODES).await;

    state
        .complete(request(false), context("myfi", "organization", "org_1"))
        .await
        .unwrap();
}

#[tokio::test]
async fn byok_is_not_gated() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 1).await;
    mount_usage_meters(&aether).await;
    Mock::given(path_regex(r"^/entitlements/|^/credits/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&aether)
        .await;

    let state = gated_state(&provider, &aether, PROD_FAIL_MODES).await;
    let mut ctx = context("facet", "user", "usr_1");
    if let Some(identity) = ctx.billing_identity.as_mut() {
        identity.mode = BillingMode::Byok;
    }
    ctx.provider_keys
        .insert("mock".to_owned(), SecretString::from("user-key".to_owned()));

    state.complete(request(false), ctx).await.unwrap();
}

fn post(uri: &str, body: &serde_json::Value) -> http::Request<Body> {
    http::Request::post(uri)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn the_refusal_renders_as_a_distinct_openai_error() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 0).await;
    mount_free_account(&aether).await;

    let router = llm_router(gated_state(&provider, &aether, PROD_FAIL_MODES).await)
        .layer(axum::Extension(context("facet", "user", "usr_1")));
    let chat = serde_json::json!({
        "model": format!("mock/{MODEL}"),
        "messages": [{ "role": "user", "content": "hello" }],
        "stream": true,
    });

    let response = router.oneshot(post("/v1/chat/completions", &chat)).await.unwrap();

    assert_eq!(response.status(), http::StatusCode::PAYMENT_REQUIRED);
    assert_eq!(
        json_body(response).await,
        serde_json::json!({
            "error": {
                "message": "AI generation uses Omni credits. Buy credits or upgrade your plan to start.",
                "type": "payment_required",
                "code": null
            }
        })
    );
}

#[tokio::test]
async fn the_refusal_renders_as_a_distinct_anthropic_error() {
    let (provider, aether) = (MockServer::start().await, MockServer::start().await);
    mount_completion(&provider, 0).await;
    mount_free_account(&aether).await;

    let router = llm_router(gated_state(&provider, &aether, PROD_FAIL_MODES).await)
        .layer(axum::Extension(context("facet", "user", "usr_1")));
    let messages = serde_json::json!({
        "model": format!("mock/{MODEL}"),
        "max_tokens": 100,
        "messages": [{ "role": "user", "content": "hello" }],
    });

    let response = router.oneshot(post("/v1/messages", &messages)).await.unwrap();

    assert_eq!(response.status(), http::StatusCode::PAYMENT_REQUIRED);
    let body = json_body(response).await;
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "payment_required");
}
