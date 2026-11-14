//! Core LLM routing crate for Synapse
//!
//! Provides a unified interface over multiple LLM providers (`OpenAI`, Anthropic,
//! Google, AWS Bedrock) with automatic model routing, format conversion, and
//! both `OpenAI`-compatible and Anthropic-compatible API endpoints.

#![allow(clippy::must_use_candidate, clippy::missing_errors_doc)]

#[cfg(all(test, feature = "http"))]
mod body_limit_tests;
#[cfg(all(test, feature = "billing", feature = "cache"))]
mod cache_tests;
pub mod convert;
#[cfg(all(test, feature = "billing", feature = "http"))]
mod cost_report_tests;
pub mod discovery;
pub mod error;
#[cfg(feature = "http")]
pub mod handler;
pub mod health;
#[cfg(test)]
mod key_tests;
pub mod protocol;
pub mod provider;
pub mod routing;
#[cfg(all(test, feature = "billing"))]
mod served_billing_tests;
#[cfg(all(test, feature = "billing"))]
mod spend_tests;
pub mod state;
#[cfg(all(test, feature = "billing"))]
mod thinking_tests;
pub mod types;

pub use error::LlmError;
#[cfg(feature = "http")]
pub use handler::llm_router;
pub use provider::{Provider, ProviderCapabilities};
pub use routing::{ModelRouter, ResolvedModel};
pub use state::LlmState;
pub use synapse_core::{DEFAULT_PRODUCT_MARGIN, MarginPolicy};
pub use types::{CompletionRequest, CompletionResponse, StreamEvent};
