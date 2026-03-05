//! Runtime feedback tracking for model performance
//!
//! Records latency, error rates, and token usage per model.
//! Sliding window for latency percentile computation. In-memory only.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;

/// Maximum samples to retain per model in the sliding window
const MAX_SAMPLES: usize = 1000;

/// Feedback record for a single request
#[derive(Debug, Clone)]
pub struct RequestFeedback {
    /// Which provider handled the request
    pub provider: String,
    /// Which model was used
    pub model: String,
    /// Request latency
    pub latency: Duration,
    /// Whether the request succeeded
    pub success: bool,
    /// Input tokens used (if known)
    pub input_tokens: Option<u32>,
    /// Output tokens used (if known)
    pub output_tokens: Option<u32>,
}

/// Per-model latency samples
struct ModelSamples {
    latencies_ms: Vec<f64>,
    total_requests: AtomicU64,
    total_errors: AtomicU64,
}

impl ModelSamples {
    fn new() -> Self {
        Self {
            latencies_ms: Vec::with_capacity(MAX_SAMPLES),
            total_requests: AtomicU64::new(0),
            total_errors: AtomicU64::new(0),
        }
    }
}

/// Track runtime performance feedback across all models
pub struct FeedbackTracker {
    models: DashMap<String, ModelSamples>,
}

impl FeedbackTracker {
    /// Create a new feedback tracker
    pub fn new() -> Self {
        Self { models: DashMap::new() }
    }

    /// Record feedback for a completed request
    pub fn record(&self, feedback: &RequestFeedback) {
        let key = format!("{}/{}", feedback.provider, feedback.model);
        let mut entry = self.models.entry(key).or_insert_with(ModelSamples::new);

        entry.total_requests.fetch_add(1, Ordering::Relaxed);

        if feedback.success {
            let latency_ms = feedback.latency.as_secs_f64() * 1000.0;

            // Sliding window: drop oldest when full
            if entry.latencies_ms.len() >= MAX_SAMPLES {
                entry.latencies_ms.remove(0);
            }
            entry.latencies_ms.push(latency_ms);
        } else {
            entry.total_errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Get latency percentiles for a model
    pub fn latency_stats(&self, provider: &str, model: &str) -> Option<LatencyStats> {
        let key = format!("{provider}/{model}");
        let entry = self.models.get(&key)?;

        if entry.latencies_ms.is_empty() {
            return None;
        }

        let mut sorted = entry.latencies_ms.clone();
        drop(entry);
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        Some(LatencyStats {
            p50: percentile(&sorted, 0.50),
            p95: percentile(&sorted, 0.95),
            p99: percentile(&sorted, 0.99),
            sample_count: sorted.len(),
        })
    }

    /// Get a snapshot of observed performance for a model
    pub fn snapshot(&self, provider: &str, model: &str) -> ModelFeedback {
        let key = format!("{provider}/{model}");
        let Some(entry) = self.models.get(&key) else {
            return ModelFeedback {
                latency_p50_ms: None,
                error_rate: None,
                sample_count: 0,
            };
        };

        let total = entry.total_requests.load(Ordering::Relaxed);
        let errors = entry.total_errors.load(Ordering::Relaxed);

        let latency_p50_ms = if entry.latencies_ms.is_empty() {
            None
        } else {
            let mut sorted = entry.latencies_ms.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            Some(percentile(&sorted, 0.50))
        };

        let error_rate = if total == 0 {
            None
        } else {
            Some(errors as f64 / total as f64)
        };

        ModelFeedback {
            latency_p50_ms,
            error_rate,
            sample_count: total as usize,
        }
    }

    /// Get the error rate for a model
    pub fn error_rate(&self, provider: &str, model: &str) -> Option<f64> {
        let key = format!("{provider}/{model}");
        let entry = self.models.get(&key)?;

        let total = entry.total_requests.load(Ordering::Relaxed);
        let errors = entry.total_errors.load(Ordering::Relaxed);
        drop(entry);

        if total == 0 {
            return None;
        }

        Some(errors as f64 / total as f64)
    }
}

impl Default for FeedbackTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Snapshot of a model's observed performance
#[derive(Debug, Clone)]
pub struct ModelFeedback {
    /// Median latency in milliseconds
    pub latency_p50_ms: Option<f64>,
    /// Fraction of requests that failed (0.0 to 1.0)
    pub error_rate: Option<f64>,
    /// Number of requests observed
    pub sample_count: usize,
}

/// Computed latency percentiles for a model
#[derive(Debug, Clone)]
pub struct LatencyStats {
    /// 50th percentile (median) in milliseconds
    pub p50: f64,
    /// 95th percentile in milliseconds
    pub p95: f64,
    /// 99th percentile in milliseconds
    pub p99: f64,
    /// Number of samples in the window
    pub sample_count: usize,
}

/// Compute a percentile from sorted values
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (p * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_computes_stats() {
        let tracker = FeedbackTracker::new();

        for ms in [10, 20, 30, 40, 50, 60, 70, 80, 90, 100] {
            tracker.record(&RequestFeedback {
                provider: "test".to_owned(),
                model: "model-1".to_owned(),
                latency: Duration::from_millis(ms),
                success: true,
                input_tokens: None,
                output_tokens: None,
            });
        }

        let stats = tracker.latency_stats("test", "model-1").unwrap();
        assert_eq!(stats.sample_count, 10);
        assert!((stats.p50 - 60.0).abs() < 1.0);
    }

    #[test]
    fn tracks_error_rate() {
        let tracker = FeedbackTracker::new();

        // 3 successes, 1 failure
        for success in [true, true, true, false] {
            tracker.record(&RequestFeedback {
                provider: "test".to_owned(),
                model: "model-1".to_owned(),
                latency: Duration::from_millis(10),
                success,
                input_tokens: None,
                output_tokens: None,
            });
        }

        let rate = tracker.error_rate("test", "model-1").unwrap();
        assert!((rate - 0.25).abs() < 0.01);
    }
}
