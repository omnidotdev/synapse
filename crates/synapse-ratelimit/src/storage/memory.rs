use std::{num::NonZeroU32, sync::Arc, time::Duration};

use governor::{Quota, RateLimiter, clock::DefaultClock, state::keyed::DashMapStateStore};

use crate::error::RateLimitError;

type KeyedLimiter = RateLimiter<String, DashMapStateStore<String>, DefaultClock>;

/// In-memory rate limiter backed by governor
#[derive(Clone)]
pub struct MemoryLimiter {
    limiter: Arc<KeyedLimiter>,
}

impl MemoryLimiter {
    /// Create a new in-memory rate limiter
    ///
    /// # Arguments
    /// * `max_requests` - Maximum requests per window
    /// * `window` - Time window duration
    pub fn new(max_requests: u32, window: Duration) -> Result<Self, RateLimitError> {
        let per_second = if window.as_secs() > 0 {
            f64::from(max_requests.max(1)) / window.as_secs_f64()
        } else {
            return Err(RateLimitError::Config("rate limit window must be > 0".to_string()));
        };

        // Convert to governor's quota format
        let replenish_interval = Duration::from_secs_f64(1.0 / per_second);
        let burst = NonZeroU32::new(max_requests.max(1))
            .ok_or_else(|| RateLimitError::Config("max_requests must be > 0".to_string()))?;

        let quota = Quota::with_period(replenish_interval)
            .ok_or_else(|| RateLimitError::Config("invalid rate limit period".to_string()))?
            .allow_burst(burst);

        let limiter = RateLimiter::dashmap(quota);

        Ok(Self {
            limiter: Arc::new(limiter),
        })
    }

    /// Check if a request is allowed for the given key
    pub fn check(&self, key: &str) -> Result<(), RateLimitError> {
        match self.limiter.check_key(&key.to_string()) {
            Ok(()) => Ok(()),
            Err(not_until) => {
                let retry_after =
                    not_until.wait_time_from(governor::clock::Clock::now(&governor::clock::DefaultClock::default()));
                Err(RateLimitError::Exceeded {
                    retry_after: retry_after.as_secs().max(1),
                })
            }
        }
    }
}
