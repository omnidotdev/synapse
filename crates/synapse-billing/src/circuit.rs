use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::BillingError;

/// Consecutive failures before the circuit opens
const FAILURE_THRESHOLD: u32 = 3;

/// How long the circuit stays open before allowing a probe request
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Circuit breaker that short-circuits billing calls when Aether is unhealthy
#[derive(Clone)]
pub struct CircuitBreaker {
    state: Arc<CircuitState>,
}

struct CircuitState {
    failure_count: AtomicU32,
    opened_at: Mutex<Option<Instant>>,
}

impl CircuitBreaker {
    /// Create a closed circuit breaker
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(CircuitState {
                failure_count: AtomicU32::new(0),
                opened_at: Mutex::new(None),
            }),
        }
    }

    /// Check whether the circuit allows a request through.
    ///
    /// # Errors
    ///
    /// Returns `BillingError::CircuitOpen` if the circuit is open and the
    /// recovery timeout has not elapsed
    pub(crate) fn check(&self) -> Result<(), BillingError> {
        let opened_at = self
            .state
            .opened_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        match *opened_at {
            // Circuit is closed
            None => Ok(()),
            // Circuit is open — allow a probe if recovery timeout elapsed
            Some(ts) if ts.elapsed() >= RECOVERY_TIMEOUT => Ok(()),
            // Circuit is open and recovery timeout has not elapsed
            Some(_) => Err(BillingError::CircuitOpen),
        }
    }

    /// Record a successful request, closing the circuit
    pub(crate) fn record_success(&self) {
        self.state.failure_count.store(0, Ordering::Relaxed);
        let mut opened_at = self
            .state
            .opened_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *opened_at = None;
    }

    /// Record a failed request, opening the circuit if the threshold is reached
    pub(crate) fn record_failure(&self) {
        let prev = self.state.failure_count.fetch_add(1, Ordering::Relaxed);

        if prev + 1 >= FAILURE_THRESHOLD {
            let mut opened_at = self
                .state
                .opened_at
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *opened_at = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_circuit_allows_requests() {
        let cb = CircuitBreaker::new();
        assert!(cb.check().is_ok());
    }

    #[test]
    fn opens_after_threshold_failures() {
        let cb = CircuitBreaker::new();

        for _ in 0..FAILURE_THRESHOLD {
            cb.record_failure();
        }

        assert!(matches!(cb.check(), Err(BillingError::CircuitOpen)));
    }

    #[test]
    fn success_resets_failure_count() {
        let cb = CircuitBreaker::new();

        // Accumulate failures just below threshold
        for _ in 0..FAILURE_THRESHOLD - 1 {
            cb.record_failure();
        }

        cb.record_success();

        // Another failure should not open the circuit
        cb.record_failure();
        assert!(cb.check().is_ok());
    }

    #[test]
    fn success_closes_open_circuit() {
        let cb = CircuitBreaker::new();

        for _ in 0..FAILURE_THRESHOLD {
            cb.record_failure();
        }

        assert!(matches!(cb.check(), Err(BillingError::CircuitOpen)));

        cb.record_success();
        assert!(cb.check().is_ok());
    }

    #[test]
    fn probe_allowed_after_recovery_timeout() {
        let cb = CircuitBreaker::new();

        for _ in 0..FAILURE_THRESHOLD {
            cb.record_failure();
        }

        // Simulate elapsed recovery timeout by backdating `opened_at`
        {
            let mut opened_at = cb.state.opened_at.lock().unwrap();
            *opened_at = Some(
                Instant::now()
                    .checked_sub(RECOVERY_TIMEOUT + Duration::from_millis(1))
                    .unwrap(),
            );
        }

        assert!(cb.check().is_ok());
    }

    #[test]
    fn probe_failure_resets_recovery_timer() {
        let cb = CircuitBreaker::new();

        for _ in 0..FAILURE_THRESHOLD {
            cb.record_failure();
        }

        // Backdate to allow probe
        {
            let mut opened_at = cb.state.opened_at.lock().unwrap();
            *opened_at = Some(
                Instant::now()
                    .checked_sub(RECOVERY_TIMEOUT + Duration::from_millis(1))
                    .unwrap(),
            );
        }

        // Probe allowed
        assert!(cb.check().is_ok());

        // Probe fails — circuit should stay open with fresh timer
        cb.record_failure();
        assert!(matches!(cb.check(), Err(BillingError::CircuitOpen)));
    }

    #[test]
    fn clone_shares_state() {
        let cb1 = CircuitBreaker::new();
        let cb2 = cb1.clone();

        for _ in 0..FAILURE_THRESHOLD {
            cb1.record_failure();
        }

        assert!(matches!(cb2.check(), Err(BillingError::CircuitOpen)));
    }
}
