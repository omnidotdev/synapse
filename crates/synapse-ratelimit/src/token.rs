use std::{collections::HashMap, convert::TryFrom, time::Duration};

use crate::{error::RateLimitError, storage::memory::MemoryLimiter};

/// Token-based rate limiter for LLM requests (per-client, per-model)
pub struct TokenLimiter {
    default_limiter: MemoryLimiter,
    default_tokens: u64,
    group_limiters: HashMap<String, (MemoryLimiter, u64)>,
}

impl TokenLimiter {
    /// Create from configuration
    pub fn new(config: &synapse_config::TokenRateLimitConfig) -> Result<Self, RateLimitError> {
        let default_window = parse_duration(&config.default.window)?;
        let default_limiter = MemoryLimiter::new(
            // Use requests-per-window as a proxy for token counting
            // The actual token counting is done at the application level
            u32::try_from(config.default.tokens)
                .map_err(|_| RateLimitError::Config("default token limit exceeds u32 range".to_string()))?,
            default_window,
        )?;

        let mut group_limiters = HashMap::new();
        for (group, token_limit) in &config.groups {
            let window = parse_duration(&token_limit.window)?;
            let tokens_u32 = u32::try_from(token_limit.tokens)
                .map_err(|_| RateLimitError::Config(format!("token limit for group '{group}' exceeds u32 range")))?;
            let limiter = MemoryLimiter::new(tokens_u32, window)?;
            group_limiters.insert(group.clone(), (limiter, token_limit.tokens));
        }

        Ok(Self {
            default_limiter,
            default_tokens: config.default.tokens,
            group_limiters,
        })
    }

    /// Check if token usage is allowed for a client
    pub fn check(&self, client_id: &str, group: Option<&str>) -> Result<(), RateLimitError> {
        // Use group-specific limiter if available
        if let Some(group_name) = group
            && let Some((limiter, _)) = self.group_limiters.get(group_name)
        {
            return limiter.check(client_id);
        }

        // Fall back to default limiter
        self.default_limiter.check(client_id)
    }

    /// Get the token limit for a client
    pub fn token_limit(&self, group: Option<&str>) -> u64 {
        if let Some(group_name) = group
            && let Some((_, tokens)) = self.group_limiters.get(group_name)
        {
            return *tokens;
        }
        self.default_tokens
    }
}

fn parse_duration(s: &str) -> Result<Duration, RateLimitError> {
    duration_str::parse(s).map_err(|e| RateLimitError::Config(format!("invalid duration '{s}': {e}")))
}
