use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use tokio::sync::mpsc;
use url::Url;

/// A token usage event to report to synapse-api
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEvent {
    /// User ID
    pub user_id: String,
    /// Workspace ID (optional)
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
    /// Cost in cents
    pub cost_cents: u32,
    /// Billing mode
    pub mode: String,
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
        let _ = self.tx.try_send(event);
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
                    // Channel closed — flush remaining and exit
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

async fn flush(http: &reqwest::Client, url: &url::Url, gateway_secret: &SecretString, buffer: &mut Vec<UsageEvent>) {
    if buffer.is_empty() {
        return;
    }

    let events = std::mem::take(buffer);
    let count = events.len();

    if let Err(e) = http
        .post(url.clone())
        .header("X-Gateway-Secret", gateway_secret.expose_secret())
        .json(&serde_json::json!({ "events": events }))
        .send()
        .await
    {
        tracing::warn!(error = %e, count, "failed to report usage");
    }
}
