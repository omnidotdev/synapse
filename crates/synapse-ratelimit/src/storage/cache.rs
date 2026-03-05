use std::time::Duration;

use crate::error::RateLimitError;

/// Cache-backed rate limiter using sliding window counters
#[derive(Clone)]
pub struct CacheLimiter {
    client: redis::Client,
    max_requests: u32,
    window: Duration,
}

impl CacheLimiter {
    /// Create a new cache-backed rate limiter
    pub fn new(url: &str, max_requests: u32, window: Duration) -> Result<Self, RateLimitError> {
        let client = redis::Client::open(url).map_err(|e| RateLimitError::Cache(format!("failed to connect: {e}")))?;

        Ok(Self {
            client,
            max_requests,
            window,
        })
    }

    /// Check if a request is allowed for the given key
    pub async fn check(&self, key: &str) -> Result<(), RateLimitError> {
        use redis::AsyncCommands;

        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| RateLimitError::Cache(format!("failed to get connection: {e}")))?;

        let rate_key = format!("synapse:ratelimit:{key}");
        let window_secs = self.window.as_secs().max(1);

        // Increment counter and set expiry atomically
        let count: u32 = redis::cmd("INCR")
            .arg(&rate_key)
            .query_async(&mut conn)
            .await
            .map_err(|e| RateLimitError::Cache(format!("INCR failed: {e}")))?;

        // Set expiry on first request in window
        if count == 1 {
            let _: () = conn
                .expire(&rate_key, i64::try_from(window_secs).unwrap_or(i64::MAX))
                .await
                .map_err(|e| RateLimitError::Cache(format!("EXPIRE failed: {e}")))?;
        }

        if count > self.max_requests {
            let ttl: i64 = conn
                .ttl(&rate_key)
                .await
                .map_err(|e| RateLimitError::Cache(format!("TTL failed: {e}")))?;

            return Err(RateLimitError::Exceeded {
                retry_after: u64::try_from(ttl.max(1)).unwrap_or(1),
            });
        }

        Ok(())
    }
}
