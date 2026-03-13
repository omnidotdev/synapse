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
}

impl Default for MeterKeys {
    fn default() -> Self {
        Self {
            input_tokens: "input_tokens".to_owned(),
            output_tokens: "output_tokens".to_owned(),
            requests: "requests".to_owned(),
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
            metadata,
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
}
