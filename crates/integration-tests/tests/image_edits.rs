//! Image edits (`POST /v1/images/edits`): multipart passthrough and billing

mod harness;

use harness::config::ConfigBuilder;
use harness::mock_llm::{CapturedPart, MOCK_EDIT_USAGE, MockLlm};
use harness::server::TestServer;
use reqwest::multipart::{Form, Part};
use wiremock::matchers::{body_partial_json, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "synapse_test_key";

/// Credits endpoints for the creator test entity
const CREDITS_CHECK: &str = "/credits/creator/user/u1/check";
const CREDITS_DEDUCT: &str = "/credits/creator/user/u1/deduct";

fn image(bytes: &[u8], file_name: &str) -> Part {
    Part::bytes(bytes.to_vec())
        .file_name(file_name.to_owned())
        .mime_str("image/png")
        .unwrap()
}

/// An edit with four references and a mask, as Creator sends it
fn edit_form(model: &str) -> Form {
    Form::new()
        .text("model", model.to_owned())
        .text("prompt", "a sticker of a fox in the reference style")
        .part("image[]", image(b"ref-one", "og.png"))
        .part("image[]", image(b"ref-two", "two.png"))
        .part("image[]", image(b"ref-three", "three.png"))
        .part("image[]", image(b"ref-four", "four.png"))
        .part("mask", image(b"mask-bytes", "mask.png"))
        .text("quality", "medium")
        .text("size", "1024x1024")
        .text("n", "1")
        .text("input_fidelity", "high")
        .text("background", "transparent")
}

fn text_part(name: &str, value: &str) -> CapturedPart {
    CapturedPart {
        name: name.to_owned(),
        file_name: None,
        content_type: None,
        data: value.as_bytes().to_vec(),
    }
}

fn file_part(name: &str, bytes: &[u8], file_name: &str) -> CapturedPart {
    CapturedPart {
        name: name.to_owned(),
        file_name: Some(file_name.to_owned()),
        content_type: Some("image/png".to_owned()),
        data: bytes.to_vec(),
    }
}

/// Mock synapse-api resolving `API_KEY` to a creator product key
async fn mock_api(key: serde_json::Value) -> MockServer {
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/internal/resolve-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(key))
        .mount(&api)
        .await;
    Mock::given(method("POST"))
        .and(path("/internal/report-usage"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&api)
        .await;
    api
}

fn creator_key(mode: &str, provider_keys: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "userId": "u1",
        "apiKeyId": "k1",
        "mode": mode,
        "plan": "free",
        "appId": "creator",
        "billingEntity": { "entityType": "user", "entityId": "u1" },
        "providerKeys": provider_keys
    })
}

/// Mock Aether granting every entitlement and usage check; credits mocks are
/// mounted per test
async fn mock_aether() -> MockServer {
    let aether = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/entitlements/.+/check$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "hasEntitlement": true })))
        .mount(&aether)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/entitlements/[^/]+/[^/]+/[^/]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "entitlements": [] })))
        .mount(&aether)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/usage/.+/check$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "allowed": true })))
        .mount(&aether)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/usage/.+/record$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "eventId": "e" })))
        .mount(&aether)
        .await;
    aether
}

async fn billed_server(mock: &MockLlm, api: &MockServer, aether: &MockServer) -> TestServer {
    let config = ConfigBuilder::new()
        .with_imagegen_provider("mock", &mock.base_url())
        .with_auth(&api.uri())
        .with_billing(&aether.uri())
        .build();
    TestServer::start(config).await.unwrap()
}

async fn send_edit(server: &TestServer, form: Form) -> reqwest::Response {
    server
        .client()
        .post(server.url("/v1/images/edits"))
        .bearer_auth(API_KEY)
        .multipart(form)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn edit_forwards_every_part_intact() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_imagegen_provider("mock", &mock.base_url())
        .build();
    let server = TestServer::start(config).await.unwrap();

    let resp = send_edit(&server, edit_form("mock/gpt-image-1.5")).await;

    assert_eq!(resp.status(), 200);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["data"][0]["b64_json"], "bW9jay1pbWFnZQ==");
    assert_eq!(json["usage"]["output_tokens"], MOCK_EDIT_USAGE.2);

    let edits = mock.image_edits();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].authorization.as_deref(), Some("Bearer test-key"));
    // Same parts in the same order, with only the provider prefix dropped
    // from the model id
    assert_eq!(
        edits[0].parts,
        vec![
            text_part("model", "gpt-image-1.5"),
            text_part("prompt", "a sticker of a fox in the reference style"),
            file_part("image[]", b"ref-one", "og.png"),
            file_part("image[]", b"ref-two", "two.png"),
            file_part("image[]", b"ref-three", "three.png"),
            file_part("image[]", b"ref-four", "four.png"),
            file_part("mask", b"mask-bytes", "mask.png"),
            text_part("quality", "medium"),
            text_part("size", "1024x1024"),
            text_part("n", "1"),
            text_part("input_fidelity", "high"),
            text_part("background", "transparent"),
        ]
    );
}

#[tokio::test]
async fn edit_accepts_reference_images_above_the_default_body_limit() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_imagegen_provider("mock", &mock.base_url())
        .build();
    let server = TestServer::start(config).await.unwrap();

    let large = vec![7_u8; 3 << 20];
    let form = Form::new()
        .text("model", "mock/gpt-image-1.5")
        .text("prompt", "a sticker")
        .part("image[]", image(&large, "one.png"))
        .part("image[]", image(&large, "two.png"));

    let resp = send_edit(&server, form).await;

    assert_eq!(resp.status(), 200);
    let edits = mock.image_edits();
    assert_eq!(edits[0].parts[2].data.len(), large.len());
    assert_eq!(edits[0].parts[3].data.len(), large.len());
}

#[tokio::test]
async fn edit_without_images_is_rejected_before_the_provider() {
    let mock = MockLlm::start().await.unwrap();
    let config = ConfigBuilder::new()
        .with_imagegen_provider("mock", &mock.base_url())
        .build();
    let server = TestServer::start(config).await.unwrap();

    let form = Form::new()
        .text("model", "mock/gpt-image-1.5")
        .text("prompt", "a sticker");
    let resp = send_edit(&server, form).await;

    assert_eq!(resp.status(), 400);
    assert!(mock.image_edits().is_empty());
}

// The mock reports 10,000 image and 50 text input tokens and 1,056 output
// tokens: $0.08 + $0.00025 + $0.033792 = $0.114042 upstream, $0.228084 at the
// 2.0 product margin, deducted rounded up to the ledger's 4 places
#[tokio::test]
async fn managed_edit_is_credit_checked_then_charged_from_reported_usage() {
    let mock = MockLlm::start().await.unwrap();
    let api = mock_api(creator_key("managed", serde_json::json!([]))).await;
    let aether = mock_aether().await;
    Mock::given(method("GET"))
        .and(path(CREDITS_CHECK))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "sufficient": true, "balance": 10.0 })),
        )
        .expect(1)
        .mount(&aether)
        .await;
    Mock::given(method("POST"))
        .and(path(CREDITS_DEDUCT))
        .and(body_partial_json(serde_json::json!({
            "amount": 0.2281,
            "referenceType": "image_edit",
            "description": "mock/gpt-image-1.5"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "success": true })))
        .expect(1)
        .mount(&aether)
        .await;
    let server = billed_server(&mock, &api, &aether).await;

    let resp = send_edit(&server, edit_form("mock/gpt-image-1.5")).await;

    assert_eq!(resp.status(), 200);
    assert_eq!(mock.image_edits().len(), 1);
    // Managed requests are served on the gateway's key
    assert_eq!(mock.image_edits()[0].authorization.as_deref(), Some("Bearer test-key"));
}

#[tokio::test]
async fn out_of_credits_edit_is_refused_with_402_before_the_provider() {
    let mock = MockLlm::start().await.unwrap();
    let api = mock_api(creator_key("managed", serde_json::json!([]))).await;
    let aether = mock_aether().await;
    Mock::given(method("GET"))
        .and(path(CREDITS_CHECK))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "sufficient": false, "balance": 0.01 })),
        )
        .expect(1)
        .mount(&aether)
        .await;
    Mock::given(method("POST"))
        .and(path(CREDITS_DEDUCT))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&aether)
        .await;
    let server = billed_server(&mock, &api, &aether).await;

    let resp = send_edit(&server, edit_form("mock/gpt-image-1.5")).await;

    assert_eq!(resp.status(), 402);
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["error"]["type"], "insufficient_credits_error");
    assert!(mock.image_edits().is_empty());
}

#[tokio::test]
async fn managed_edit_for_an_unpriced_model_is_refused_before_the_provider() {
    let mock = MockLlm::start().await.unwrap();
    let api = mock_api(creator_key("managed", serde_json::json!([]))).await;
    let aether = mock_aether().await;
    Mock::given(path_regex(r"^/credits/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&aether)
        .await;
    let server = billed_server(&mock, &api, &aether).await;

    let resp = send_edit(&server, edit_form("mock/dall-e-2")).await;

    assert_eq!(resp.status(), 400);
    assert!(mock.image_edits().is_empty());
}

#[tokio::test]
async fn byok_edit_uses_the_callers_key_and_is_never_charged() {
    let mock = MockLlm::start().await.unwrap();
    let api = mock_api(creator_key(
        "byok",
        serde_json::json!([{ "provider": "mock", "decryptedKey": "sk-user-own" }]),
    ))
    .await;
    let aether = mock_aether().await;
    Mock::given(path_regex(r"^/credits/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&aether)
        .await;
    let server = billed_server(&mock, &api, &aether).await;

    let resp = send_edit(&server, edit_form("mock/gpt-image-1.5")).await;

    assert_eq!(resp.status(), 200);
    let edits = mock.image_edits();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].authorization.as_deref(), Some("Bearer sk-user-own"));
}

#[tokio::test]
async fn byok_edit_without_a_provider_key_is_refused_not_served_on_the_gateway_key() {
    let mock = MockLlm::start().await.unwrap();
    let api = mock_api(creator_key("byok", serde_json::json!([]))).await;
    let aether = mock_aether().await;
    let server = billed_server(&mock, &api, &aether).await;

    let resp = send_edit(&server, edit_form("mock/gpt-image-1.5")).await;

    assert_eq!(resp.status(), 400);
    assert!(mock.image_edits().is_empty());
}
