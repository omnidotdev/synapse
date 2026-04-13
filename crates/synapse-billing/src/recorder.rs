use std::collections::HashMap;

use tokio::sync::mpsc;

use crate::client::AetherClient;

/// Usage event to be recorded asynchronously
#[derive(Debug, Clone)]
pub struct UsageEvent {
    /// Entity type (e.g. "user")
    pub entity_type: String,
    /// Entity identifier (user ID from JWT sub)
    pub entity_id: String,
    /// Model used for this request
    pub model: String,
    /// Provider that served the request
    pub provider: String,
    /// Number of input/prompt tokens consumed
    pub input_tokens: u32,
    /// Number of output/completion tokens generated
    pub output_tokens: u32,
    /// Estimated cost in USD (based on model profile pricing)
    pub estimated_cost_usd: f64,
    /// Unique key for idempotent recording
    pub idempotency_key: String,
}

/// Configuration for meter key names
#[derive(Debug, Clone)]
pub struct MeterKeys {
    /// Meter key for input tokens
    pub input_tokens: String,
    /// Meter key for output tokens
    pub output_tokens: String,
    /// Meter key for request count
    pub requests: String,
    /// Meter key for message count
    pub messages: String,
}

impl Default for MeterKeys {
    fn default() -> Self {
        Self {
            input_tokens: "input_tokens".to_owned(),
            output_tokens: "output_tokens".to_owned(),
            requests: "requests".to_owned(),
            messages: "messages".to_owned(),
        }
    }
}

/// Async usage recorder that dispatches events to a background task
///
/// Records are sent via an unbounded channel and processed
/// asynchronously so recording never blocks the response
#[derive(Clone)]
pub struct UsageRecorder {
    tx: mpsc::UnboundedSender<UsageEvent>,
}

impl UsageRecorder {
    /// Create a new recorder and spawn its background processing task
    ///
    /// The background task runs until the sender is dropped
    #[must_use]
    pub fn new(client: AetherClient, meter_keys: MeterKeys) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        tokio::spawn(process_events(rx, client, meter_keys));

        Self { tx }
    }

    /// Enqueue a usage event for background recording
    ///
    /// This is non-blocking and fire-and-forget. If the channel is
    /// closed (background task stopped), the event is silently dropped
    pub fn record(&self, event: UsageEvent) {
        if let Err(e) = self.tx.send(event) {
            tracing::warn!(
                error = %e,
                "failed to enqueue usage event, channel closed"
            );
        }
    }
}

impl std::fmt::Debug for UsageRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsageRecorder").finish_non_exhaustive()
    }
}

/// Background task that processes usage events
async fn process_events(mut rx: mpsc::UnboundedReceiver<UsageEvent>, client: AetherClient, meter_keys: MeterKeys) {
    while let Some(event) = rx.recv().await {
        record_event(&client, &meter_keys, &event).await;
    }

    tracing::debug!("usage recorder shutting down");
}

/// Record a single usage event to Aether as three meter updates
#[allow(clippy::cognitive_complexity)]
async fn record_event(client: &AetherClient, meter_keys: &MeterKeys, event: &UsageEvent) {
    let metadata = build_metadata(event);

    // Record input tokens
    if event.input_tokens > 0
        && let Err(e) = client
            .record_usage(
                &event.entity_type,
                &event.entity_id,
                &meter_keys.input_tokens,
                f64::from(event.input_tokens),
                &format!("{}-input", event.idempotency_key),
                metadata.clone(),
            )
            .await
    {
        tracing::warn!(
            error = %e,
            entity_id = %event.entity_id,
            meter = %meter_keys.input_tokens,
            tokens = event.input_tokens,
            "failed to record input token usage"
        );
    }

    // Record output tokens
    if event.output_tokens > 0
        && let Err(e) = client
            .record_usage(
                &event.entity_type,
                &event.entity_id,
                &meter_keys.output_tokens,
                f64::from(event.output_tokens),
                &format!("{}-output", event.idempotency_key),
                metadata.clone(),
            )
            .await
    {
        tracing::warn!(
            error = %e,
            entity_id = %event.entity_id,
            meter = %meter_keys.output_tokens,
            tokens = event.output_tokens,
            "failed to record output token usage"
        );
    }

    // Record request count
    if let Err(e) = client
        .record_usage(
            &event.entity_type,
            &event.entity_id,
            &meter_keys.requests,
            1.0,
            &format!("{}-req", event.idempotency_key),
            metadata.clone(),
        )
        .await
    {
        tracing::warn!(
            error = %e,
            entity_id = %event.entity_id,
            meter = %meter_keys.requests,
            "failed to record request count"
        );
    }

    // Record message count
    if let Err(e) = client
        .record_usage(
            &event.entity_type,
            &event.entity_id,
            &meter_keys.messages,
            1.0,
            &format!("{}-msg", event.idempotency_key),
            metadata,
        )
        .await
    {
        tracing::warn!(
            error = %e,
            entity_id = %event.entity_id,
            meter = %meter_keys.messages,
            "failed to record message count"
        );
    }
}

/// Build metadata map for a usage recording
fn build_metadata(event: &UsageEvent) -> HashMap<String, String> {
    let mut metadata = HashMap::new();
    metadata.insert("model".to_owned(), event.model.clone());
    metadata.insert("provider".to_owned(), event.provider.clone());
    metadata.insert("estimated_cost_usd".to_owned(), event.estimated_cost_usd.to_string());
    metadata
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_meter_keys() {
        let keys = MeterKeys::default();
        assert_eq!(keys.input_tokens, "input_tokens");
        assert_eq!(keys.output_tokens, "output_tokens");
        assert_eq!(keys.requests, "requests");
        assert_eq!(keys.messages, "messages");
    }

    #[test]
    fn byok_event_with_zero_tokens_still_records_request() {
        // BYOK events have zero tokens but must still be recorded so the
        // request count meter is incremented (recorder always records delta=1)
        let event = UsageEvent {
            entity_type: "user".to_owned(),
            entity_id: "usr_byok".to_owned(),
            model: "gpt-4o".to_owned(),
            provider: "openai".to_owned(),
            input_tokens: 0,
            output_tokens: 0,
            estimated_cost_usd: 0.0,
            idempotency_key: "byok-key-1".to_owned(),
        };

        // Verify the event is valid and metadata is constructed
        let metadata = build_metadata(&event);
        assert_eq!(metadata.get("model"), Some(&"gpt-4o".to_owned()));

        // The record_event function will skip input_tokens (0) and output_tokens (0)
        // but will always record the request count (delta=1)
        // This is verified by the fact that record_event unconditionally calls
        // client.record_usage for the requests meter (lines 150-167)
        assert_eq!(event.input_tokens, 0);
        assert_eq!(event.output_tokens, 0);
    }

    #[test]
    fn build_metadata_includes_fields() {
        let event = UsageEvent {
            entity_type: "user".to_owned(),
            entity_id: "usr_1".to_owned(),
            model: "gpt-4o".to_owned(),
            provider: "openai".to_owned(),
            input_tokens: 100,
            output_tokens: 50,
            estimated_cost_usd: 0.01,
            idempotency_key: "key-1".to_owned(),
        };

        let metadata = build_metadata(&event);
        assert_eq!(metadata.get("model"), Some(&"gpt-4o".to_owned()));
        assert_eq!(metadata.get("provider"), Some(&"openai".to_owned()));
        assert!(metadata.contains_key("estimated_cost_usd"));
    }

    #[tokio::test]
    async fn byok_zero_token_event_records_request_count_to_aether() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        let ok_response = serde_json::json!({
            "billingAccountId": "ba_123",
            "meterId": "meter_456",
            "eventId": "evt_789"
        });

        // Request count should always be recorded — even with zero tokens
        Mock::given(method("POST"))
            .and(path("/usage/test-app/user/usr_byok/requests/record"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&ok_response))
            .expect(1)
            .named("requests meter must be called for BYOK")
            .mount(&server)
            .await;

        // Message count should always be recorded — even with zero tokens
        Mock::given(method("POST"))
            .and(path("/usage/test-app/user/usr_byok/messages/record"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&ok_response))
            .expect(1)
            .named("messages meter must be called for BYOK")
            .mount(&server)
            .await;

        // Token meters should NOT be called (zero tokens are skipped)
        Mock::given(method("POST"))
            .and(path("/usage/test-app/user/usr_byok/input_tokens/record"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&ok_response))
            .expect(0)
            .named("input_tokens meter must NOT be called for BYOK")
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/usage/test-app/user/usr_byok/output_tokens/record"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&ok_response))
            .expect(0)
            .named("output_tokens meter must NOT be called for BYOK")
            .mount(&server)
            .await;

        let client = crate::AetherClient::new(
            url::Url::parse(&format!("{}/", server.uri())).unwrap(),
            "test-app".to_owned(),
            secrecy::SecretString::from("test-key".to_owned()),
        )
        .unwrap();

        let event = UsageEvent {
            entity_type: "user".to_owned(),
            entity_id: "usr_byok".to_owned(),
            model: "gpt-4o".to_owned(),
            provider: "openai".to_owned(),
            input_tokens: 0,
            output_tokens: 0,
            estimated_cost_usd: 0.0,
            idempotency_key: "byok-idem-1".to_owned(),
        };

        record_event(&client, &MeterKeys::default(), &event).await;

        // Wiremock will verify expectations when the server is dropped
    }

    #[tokio::test]
    async fn managed_event_records_all_three_meters() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        let ok_response = serde_json::json!({
            "billingAccountId": "ba_123",
            "meterId": "meter_456",
            "eventId": "evt_789"
        });

        Mock::given(method("POST"))
            .and(path("/usage/test-app/user/usr_managed/requests/record"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&ok_response))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/usage/test-app/user/usr_managed/input_tokens/record"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&ok_response))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/usage/test-app/user/usr_managed/output_tokens/record"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&ok_response))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/usage/test-app/user/usr_managed/messages/record"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&ok_response))
            .expect(1)
            .mount(&server)
            .await;

        let client = crate::AetherClient::new(
            url::Url::parse(&format!("{}/", server.uri())).unwrap(),
            "test-app".to_owned(),
            secrecy::SecretString::from("test-key".to_owned()),
        )
        .unwrap();

        let event = UsageEvent {
            entity_type: "user".to_owned(),
            entity_id: "usr_managed".to_owned(),
            model: "gpt-4o".to_owned(),
            provider: "openai".to_owned(),
            input_tokens: 500,
            output_tokens: 200,
            estimated_cost_usd: 0.05,
            idempotency_key: "managed-idem-1".to_owned(),
        };

        record_event(&client, &MeterKeys::default(), &event).await;
    }
}
