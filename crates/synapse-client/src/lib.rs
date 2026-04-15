#![allow(clippy::must_use_candidate, clippy::missing_errors_doc)]

//! Typed Rust HTTP client for the Synapse AI router
//!
//! Provides a high-level interface to Synapse's LLM, MCP, STT, and TTS
//! endpoints with streaming support

#[cfg(feature = "agent-core")]
mod agent_provider;
mod client;
#[cfg(feature = "embedded")]
mod embedded;
pub mod error;
pub mod types;

pub use client::SynapseClient;
pub use error::{Result, SynapseClientError};
pub use types::*;
