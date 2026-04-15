#![allow(clippy::missing_errors_doc, clippy::must_use_candidate)]

mod error;
mod request;
pub mod storage;
mod token;

pub use error::RateLimitError;
pub use request::RequestLimiter;
pub use token::TokenLimiter;

use synapse_config::RateLimitConfig;

/// Create a request limiter from configuration
pub fn create_request_limiter(config: &RateLimitConfig) -> Result<RequestLimiter, RateLimitError> {
    RequestLimiter::new(config)
}

/// Create a token limiter from configuration
pub fn create_token_limiter(config: &synapse_config::TokenRateLimitConfig) -> Result<TokenLimiter, RateLimitError> {
    TokenLimiter::new(config)
}
