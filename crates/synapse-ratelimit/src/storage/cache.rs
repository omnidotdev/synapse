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

    /// Check if a request is allowed for the given key.
    /// Uses a Lua script to atomically increment and set expiry.
    pub async fn check(&self, key: &str) -> Result<(), RateLimitError> {
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| RateLimitError::Cache(format!("failed to get connection: {e}")))?;

        let rate_key = format!("synapse:ratelimit:{key}");
        let window_secs = self.window.as_secs().max(1);

        // Atomic increment + conditional expire via Lua
        let script = redis::Script::new(
            r"
            local count = redis.call('INCR', KEYS[1])
            if count == 1 then
                redis.call('EXPIRE', KEYS[1], ARGV[1])
            end
            return count
            ",
        );

        let count: u32 = script
            .key(&rate_key)
            .arg(window_secs)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| RateLimitError::Cache(format!("rate limit script failed: {e}")))?;

        if count > self.max_requests {
            use redis::AsyncCommands;

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
