//! Core LLM routing crate for Synapse
//!
//! Provides a unified interface over multiple LLM providers (`OpenAI`, Anthropic,
//! Google, AWS Bedrock) with automatic model routing, format conversion, and
//! both `OpenAI`-compatible and Anthropic-compatible API endpoints.

#![allow(clippy::must_use_candidate, clippy::missing_errors_doc)]

pub mod convert;
pub mod discovery;
pub mod error;
#[cfg(feature = "http")]
pub mod handler;
pub mod health;
pub mod protocol;
pub mod provider;
pub mod routing;
pub mod state;
pub mod types;

pub use error::LlmError;
#[cfg(feature = "http")]
pub use handler::llm_router;
pub use provider::{Provider, ProviderCapabilities};
pub use routing::{ModelRouter, ResolvedModel};
pub use state::LlmState;
pub use types::{CompletionRequest, CompletionResponse, StreamEvent};
