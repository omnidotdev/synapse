use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use synapse_core::{ServedUsage, UsageReportSink};
use tokio::sync::mpsc;
use url::Url;

use crate::{KeyMode, ResolvedKey};

/// A served request's usage, reported to synapse-api
///
/// Field names and types follow synapse-api's `POST /internal/report-usage`
/// body schema exactly. That schema rejects a `null` where it expects a
/// string, so an absent value is omitted, never sent as `null`
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEvent {
    /// Unique id, so a retried flush records the event once
    pub event_id: String,
    /// User ID
    pub user_id: String,
    /// Workspace ID (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// API key ID
    pub api_key_id: String,
    /// Provider name
    pub provider: String,
    /// Model name
    pub model: String,
    /// Input token count
    pub input_tokens: u32,
    /// Output token count
    pub output_tokens: u32,
    /// Retail cost charged, in cents to two places (the ledger's $0.0001)
    pub cost_cents: f64,
    /// Billing mode
    pub mode: String,
    /// Kind of request served, named like the charge's reference type
    pub modality: String,
    /// When the request was served (RFC 3339, millisecond precision)
    pub occurred_at: String,
}

impl UsageEvent {
    /// The report for a request served on `key`
    #[must_use]
    pub fn served(key: &ResolvedKey, usage: ServedUsage) -> Self {
        let mode = match key.mode {
            KeyMode::Byok => "byok",
            KeyMode::Managed | KeyMode::Manual => "managed",
        };

        Self {
            event_id: uuid::Uuid::new_v4().to_string(),
            user_id: key.user_id.clone(),
            workspace_id: key.workspace_id.clone(),
            api_key_id: key.api_key_id.clone(),
            provider: usage.provider,
            model: usage.model,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cost_cents: usd_to_cents(usage.charged_usd),
            mode: mode.to_owned(),
            modality: usage.modality.as_str().to_owned(),
            occurred_at: now_rfc3339(),
        }
    }
}

/// A charged USD amount in cents to two places
///
/// The amount is already at the ledger's $0.0001 precision, so this only
/// changes the unit (and drops float noise); anything negative or not finite
/// reports as 0
fn usd_to_cents(usd: f64) -> f64 {
    if usd.is_finite() && usd > 0.0 {
        (usd * 10_000.0).round() / 100.0
    } else {
        0.0
    }
}

/// The current time, RFC 3339 with millisecond precision
fn now_rfc3339() -> String {
    jiff::Timestamp::now()
        .round(jiff::Unit::Millisecond)
        .unwrap_or_else(|_| jiff::Timestamp::now())
        .to_string()
}

/// Async usage reporter that batches events
#[derive(Clone)]
pub struct UsageReporter {
    tx: mpsc::Sender<UsageEvent>,
}

impl UsageReporter {
    /// Spawn the background reporter task
    ///
    /// Returns a handle for sending usage events. Events are batched
    /// and flushed to synapse-api every `flush_interval`.
    #[must_use]
    pub fn spawn(api_url: Url, gateway_secret: SecretString, flush_interval: Duration) -> Self {
        let (tx, rx) = mpsc::channel(10_000);
        tokio::spawn(flush_loop(api_url, gateway_secret, rx, flush_interval));
        Self { tx }
    }

    /// Record a usage event (non-blocking, drops if channel full)
    pub fn record(&self, event: UsageEvent) {
        if self.tx.try_send(event).is_err() {
            tracing::warn!("usage report queue full, dropping a usage event");
        }
    }

    /// A sink reporting each request served on `key` through this reporter
    #[must_use]
    pub fn sink_for(&self, key: std::sync::Arc<ResolvedKey>) -> UsageReportSink {
        let reporter = self.clone();
        UsageReportSink::new(move |usage| reporter.record(UsageEvent::served(&key, usage)))
    }
}

async fn flush_loop(
    api_url: Url,
    gateway_secret: SecretString,
    mut rx: mpsc::Receiver<UsageEvent>,
    interval: Duration,
) {
    let http = reqwest::Client::new();
    let url = api_url.join("/internal/report-usage").expect("valid URL join");

    let mut buffer: Vec<UsageEvent> = Vec::new();
    let mut ticker = tokio::time::interval(interval);

    loop {
        let should_flush = tokio::select! {
            msg = rx.recv() => {
                if let Some(event) = msg {
                    buffer.push(event);
                    false
                } else {
                    // Channel closed, flush remaining and exit
                    flush(&http, &url, &gateway_secret, &mut buffer).await;
                    break;
                }
            }
            _ = ticker.tick() => true,
        };

        if should_flush {
            flush(&http, &url, &gateway_secret, &mut buffer).await;
        }
    }

    tracing::info!("usage reporter shut down");
}

/// Maximum events to retain on flush failure before dropping oldest
const MAX_BUFFER_SIZE: usize = 50_000;

async fn flush(http: &reqwest::Client, url: &url::Url, gateway_secret: &SecretString, buffer: &mut Vec<UsageEvent>) {
    if buffer.is_empty() {
        return;
    }

    let events = std::mem::take(buffer);
    let count = events.len();

    let result = http
        .post(url.clone())
        .header("X-Gateway-Secret", gateway_secret.expose_secret())
        .json(&serde_json::json!({ "events": events }))
        .send()
        .await;

    match FlushOutcome::of(&result) {
        FlushOutcome::Delivered => {
            tracing::debug!(count, "reported usage");
        }
        FlushOutcome::Rejected(status) => {
            // A rejected batch fails identically on every retry, so re-queuing
            // it would only block later events behind it
            tracing::error!(%status, count, "synapse-api rejected a usage report, dropping its events");
        }
        FlushOutcome::Retry => {
            match &result {
                Ok(response) => {
                    tracing::warn!(status = %response.status(), count, "failed to report usage, re-queuing events");
                }
                Err(e) => tracing::warn!(error = %e, count, "failed to report usage, re-queuing events"),
            }
            // Re-queue failed events for the next flush attempt
            buffer.extend(events);
            // Prevent unbounded growth during extended outages
            if buffer.len() > MAX_BUFFER_SIZE {
                let excess = buffer.len() - MAX_BUFFER_SIZE;
                buffer.drain(..excess);
                tracing::warn!(dropped = excess, "usage buffer overflow, dropping oldest events");
            }
        }
    }
}

/// What a flush's response means for its events
#[derive(Debug, PartialEq, Eq)]
enum FlushOutcome {
    /// Recorded
    Delivered,
    /// Refused as invalid: retrying the same batch cannot succeed
    Rejected(reqwest::StatusCode),
    /// Transient (unreachable, overloaded, server error): retry later
    Retry,
}

impl FlushOutcome {
    fn of(result: &Result<reqwest::Response, reqwest::Error>) -> Self {
        let Ok(response) = result else {
            return Self::Retry;
        };
        let status = response.status();
        if status.is_success() {
            Self::Delivered
        } else if status.is_client_error()
            && status != reqwest::StatusCode::REQUEST_TIMEOUT
            && status != reqwest::StatusCode::TOO_MANY_REQUESTS
            && status != reqwest::StatusCode::UNAUTHORIZED
        {
            Self::Rejected(status)
        } else {
            Self::Retry
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use synapse_core::Modality;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn key(workspace_id: Option<&str>, mode: &str) -> ResolvedKey {
        serde_json::from_value(serde_json::json!({
            "userId": "11111111-1111-4111-8111-111111111111",
            "workspaceId": workspace_id,
            "apiKeyId": "22222222-2222-4222-8222-222222222222",
            "mode": mode,
            "plan": "pro"
        }))
        .expect("valid resolved key")
    }

    fn served(charged_usd: f64) -> ServedUsage {
        ServedUsage {
            modality: Modality::Transcription,
            provider: "openai".to_owned(),
            model: "whisper-1".to_owned(),
            input_tokens: 32_000,
            output_tokens: 0,
            charged_usd,
        }
    }

    fn event() -> UsageEvent {
        UsageEvent::served(&key(None, "managed"), served(0.0037))
    }

    /// synapse-api's body schema rejects `null` for an optional string, which
    /// dropped every report from a key outside a workspace
    #[test]
    fn an_absent_workspace_is_omitted_never_null() {
        let body = serde_json::to_value(event()).unwrap();

        assert!(body.get("workspaceId").is_none(), "{body}");
        assert!(
            body.as_object().unwrap().values().all(|v| !v.is_null()),
            "no field may be null: {body}"
        );

        let scoped = UsageEvent::served(&key(Some("ws_1"), "managed"), served(0.0));
        assert_eq!(serde_json::to_value(scoped).unwrap()["workspaceId"], "ws_1");
    }

    #[test]
    fn the_event_carries_what_synapse_api_records() {
        let body = serde_json::to_value(event()).unwrap();

        assert_eq!(body["userId"], "11111111-1111-4111-8111-111111111111");
        assert_eq!(body["apiKeyId"], "22222222-2222-4222-8222-222222222222");
        assert_eq!(body["mode"], "managed");
        assert_eq!(body["modality"], "transcription");
        assert_eq!(body["inputTokens"], 32_000);
        assert_eq!(body["costCents"], 0.37);
        assert!(uuid::Uuid::parse_str(body["eventId"].as_str().unwrap()).is_ok());
        assert!(body["occurredAt"].as_str().unwrap().parse::<jiff::Timestamp>().is_ok());
    }

    #[test]
    fn every_event_gets_its_own_id() {
        assert_ne!(event().event_id, event().event_id);
    }

    #[test]
    fn cost_is_reported_in_cents_at_ledger_precision() {
        assert!((usd_to_cents(0.0001) - 0.01).abs() < 1e-12);
        assert!((usd_to_cents(0.1234) - 12.34).abs() < 1e-12);
        assert!((usd_to_cents(0.3) - 30.0).abs() < 1e-12);
        assert!(usd_to_cents(0.0).abs() < f64::EPSILON);
        assert!(usd_to_cents(-1.0).abs() < f64::EPSILON);
        assert!(usd_to_cents(f64::NAN).abs() < f64::EPSILON);
    }

    #[test]
    fn a_byok_key_reports_as_byok() {
        let body = serde_json::to_value(UsageEvent::served(&key(None, "byok"), served(0.0))).unwrap();
        assert_eq!(body["mode"], "byok");
    }

    fn outcome(status: u16) -> FlushOutcome {
        let response = http::Response::builder().status(status).body("").unwrap();
        FlushOutcome::of(&Ok(reqwest::Response::from(response)))
    }

    #[test]
    fn an_invalid_batch_is_dropped_and_a_transient_failure_retried() {
        assert_eq!(outcome(200), FlushOutcome::Delivered);
        assert_eq!(
            outcome(422),
            FlushOutcome::Rejected(reqwest::StatusCode::UNPROCESSABLE_ENTITY)
        );
        assert_eq!(outcome(400), FlushOutcome::Rejected(reqwest::StatusCode::BAD_REQUEST));
        for status in [401, 408, 429, 500, 502, 503] {
            assert_eq!(outcome(status), FlushOutcome::Retry, "{status}");
        }
    }

    async fn flush_once(status: u16) -> (MockServer, Vec<UsageEvent>) {
        let api = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/report-usage"))
            .and(header("X-Gateway-Secret", "secret"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&api)
            .await;

        let url = Url::parse(&api.uri()).unwrap().join("/internal/report-usage").unwrap();
        let mut buffer = vec![event()];
        flush(
            &reqwest::Client::new(),
            &url,
            &SecretString::from("secret"),
            &mut buffer,
        )
        .await;
        (api, buffer)
    }

    #[tokio::test]
    async fn a_delivered_batch_leaves_the_buffer() {
        let (api, buffer) = flush_once(200).await;
        assert!(buffer.is_empty());
        assert_eq!(api.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_server_error_is_requeued() {
        let (_api, buffer) = flush_once(503).await;
        assert_eq!(buffer.len(), 1);
    }

    /// A rejected batch used to count as delivered (only transport errors
    /// were checked), which hid that every report was being refused
    #[tokio::test]
    async fn a_rejected_batch_is_not_requeued() {
        let (_api, buffer) = flush_once(422).await;
        assert!(buffer.is_empty());
    }

    #[tokio::test]
    async fn a_sink_reports_through_the_reporter() {
        let api = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/report-usage"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&api)
            .await;

        let reporter = UsageReporter::spawn(
            Url::parse(&api.uri()).unwrap(),
            SecretString::from("secret"),
            Duration::from_millis(20),
        );
        reporter.sink_for(Arc::new(key(None, "managed"))).report(served(0.0001));

        let mut events = Vec::new();
        for _ in 0..100 {
            events = api
                .received_requests()
                .await
                .unwrap_or_default()
                .iter()
                .flat_map(|r| {
                    serde_json::from_slice::<serde_json::Value>(&r.body).unwrap()["events"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>();
            if !events.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["modality"], "transcription");
        assert_eq!(events[0]["costCents"], 0.01);
    }
}
