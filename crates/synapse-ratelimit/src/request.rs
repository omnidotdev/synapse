use std::time::Duration;

use synapse_config::{RateLimitConfig, RateLimitStorage, RequestRateLimit};

use crate::{
    error::RateLimitError,
    storage::{cache::CacheLimiter, memory::MemoryLimiter},
};

/// HTTP request-level rate limiter (global and per-IP)
pub struct RequestLimiter {
    global: Option<Limiter>,
    per_ip: Option<Limiter>,
}

enum Limiter {
    Memory(MemoryLimiter),
    Cache(CacheLimiter),
}

impl RequestLimiter {
    /// Create from configuration
    pub fn new(config: &RateLimitConfig) -> Result<Self, RateLimitError> {
        let global = config
            .global
            .as_ref()
            .map(|rl| build_limiter(&config.storage, rl))
            .transpose()?;

        let per_ip = config
            .per_ip
            .as_ref()
            .map(|rl| build_limiter(&config.storage, rl))
            .transpose()?;

        Ok(Self { global, per_ip })
    }

    /// Check global rate limit
    pub async fn check_global(&self) -> Result<(), RateLimitError> {
        if let Some(ref limiter) = self.global {
            check_limiter(limiter, "global").await?;
        }
        Ok(())
    }

    /// Check per-IP rate limit
    pub async fn check_ip(&self, ip: &str) -> Result<(), RateLimitError> {
        if let Some(ref limiter) = self.per_ip {
            check_limiter(limiter, ip).await?;
        }
        Ok(())
    }
}

fn build_limiter(storage: &RateLimitStorage, rate_limit: &RequestRateLimit) -> Result<Limiter, RateLimitError> {
    let window = parse_duration(&rate_limit.window)?;

    match storage {
        RateLimitStorage::Memory => Ok(Limiter::Memory(MemoryLimiter::new(rate_limit.requests, window)?)),
        RateLimitStorage::Cache(cache_config) => Ok(Limiter::Cache(CacheLimiter::new(
            cache_config.url.as_str(),
            rate_limit.requests,
            window,
        )?)),
    }
}

async fn check_limiter(limiter: &Limiter, key: &str) -> Result<(), RateLimitError> {
    match limiter {
        Limiter::Memory(m) => m.check(key),
        Limiter::Cache(r) => r.check(key).await,
    }
}

fn parse_duration(s: &str) -> Result<Duration, RateLimitError> {
    duration_str::parse(s).map_err(|e| RateLimitError::Config(format!("invalid duration '{s}': {e}")))
}
