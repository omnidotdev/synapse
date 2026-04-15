//! Provider health tracking with circuit breaker pattern
//!
//! Tracks provider health and prevents sending requests to providers
//! that are consistently failing, allowing them time to recover.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use synapse_config::CircuitBreakerConfig;

/// Circuit breaker state for a provider
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Normal operation, requests flow through
    Closed,
    /// Provider is failing, requests are blocked
    Open,
    /// Probing: allow one request to test recovery
    HalfOpen,
}

/// Per-provider health state
struct ProviderHealth {
    /// Number of errors in the current window
    error_count: AtomicU32,
    /// Start of the current error window (unix timestamp seconds)
    window_start: AtomicU64,
    /// When the circuit was opened (unix timestamp seconds, 0 = not open)
    opened_at: AtomicU64,
}

impl ProviderHealth {
    fn new() -> Self {
        Self {
            error_count: AtomicU32::new(0),
            window_start: AtomicU64::new(now_secs()),
            opened_at: AtomicU64::new(0),
        }
    }
}

/// Track provider health and implement circuit breaker logic
pub struct ProviderHealthTracker {
    providers: DashMap<String, ProviderHealth>,
    config: CircuitBreakerConfig,
}

impl ProviderHealthTracker {
    /// Create a new health tracker with the given configuration
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            providers: DashMap::new(),
            config,
        }
    }

    /// Check the circuit state for a provider
    pub fn state(&self, provider: &str) -> CircuitState {
        let Some(health) = self.providers.get(provider) else {
            return CircuitState::Closed;
        };

        let opened_at = health.opened_at.load(Ordering::Relaxed);
        if opened_at == 0 {
            return CircuitState::Closed;
        }

        let elapsed = now_secs().saturating_sub(opened_at);
        if elapsed >= self.config.recovery_seconds {
            CircuitState::HalfOpen
        } else {
            CircuitState::Open
        }
    }

    /// Whether a provider is available for requests
    pub fn is_available(&self, provider: &str) -> bool {
        self.state(provider) != CircuitState::Open
    }

    /// Record a successful request to a provider
    pub fn record_success(&self, provider: &str) {
        let health = self
            .providers
            .entry(provider.to_owned())
            .or_insert_with(ProviderHealth::new);

        // Reset circuit on success (closes half-open)
        health.opened_at.store(0, Ordering::Relaxed);
        health.error_count.store(0, Ordering::Relaxed);
        health.window_start.store(now_secs(), Ordering::Relaxed);
    }

    /// Record a failed request to a provider
    pub fn record_failure(&self, provider: &str) {
        let health = self
            .providers
            .entry(provider.to_owned())
            .or_insert_with(ProviderHealth::new);

        let now = now_secs();
        let window_start = health.window_start.load(Ordering::Relaxed);

        // Reset window if expired
        if now.saturating_sub(window_start) >= self.config.window_seconds {
            health.error_count.store(1, Ordering::Relaxed);
            health.window_start.store(now, Ordering::Relaxed);
        } else {
            let count = health.error_count.fetch_add(1, Ordering::Relaxed) + 1;

            // Trip the breaker if threshold exceeded
            if count >= self.config.error_threshold {
                health.opened_at.store(now, Ordering::Relaxed);
                drop(health);
                tracing::warn!(provider, error_count = count, "circuit breaker opened for provider");
            }
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            error_threshold: 3,
            window_seconds: 60,
            recovery_seconds: 5,
        }
    }

    #[test]
    fn healthy_provider_is_closed() {
        let tracker = ProviderHealthTracker::new(test_config());
        assert_eq!(tracker.state("test"), CircuitState::Closed);
        assert!(tracker.is_available("test"));
    }

    #[test]
    fn failures_below_threshold_stay_closed() {
        let tracker = ProviderHealthTracker::new(test_config());
        tracker.record_failure("test");
        tracker.record_failure("test");
        assert_eq!(tracker.state("test"), CircuitState::Closed);
    }

    #[test]
    fn failures_at_threshold_open_circuit() {
        let tracker = ProviderHealthTracker::new(test_config());
        for _ in 0..3 {
            tracker.record_failure("test");
        }
        assert_eq!(tracker.state("test"), CircuitState::Open);
        assert!(!tracker.is_available("test"));
    }

    #[test]
    fn success_resets_circuit() {
        let tracker = ProviderHealthTracker::new(test_config());
        for _ in 0..3 {
            tracker.record_failure("test");
        }
        assert_eq!(tracker.state("test"), CircuitState::Open);

        tracker.record_success("test");
        assert_eq!(tracker.state("test"), CircuitState::Closed);
        assert!(tracker.is_available("test"));
    }

    #[test]
    fn independent_provider_tracking() {
        let tracker = ProviderHealthTracker::new(test_config());
        for _ in 0..3 {
            tracker.record_failure("bad");
        }
        assert!(!tracker.is_available("bad"));
        assert!(tracker.is_available("good"));
    }
}
