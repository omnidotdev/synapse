#![allow(clippy::must_use_candidate)]

pub mod auth;
pub mod billing;
pub mod cache;
pub mod client_identification;
pub mod client_ip;
pub mod cors;
pub mod csrf;
pub mod embeddings;
mod env;
pub mod guardrails;
pub mod headers;
pub mod health;
pub mod imagegen;
pub mod llm;
mod loader;
pub mod mcp;
pub mod oauth;
pub mod proxy;
pub mod rate_limit;
pub mod server;
pub mod stt;
pub mod telemetry;
pub mod tls;
pub mod tts;

use serde::Deserialize;

pub use auth::*;
pub use billing::*;
pub use cache::*;
pub use client_identification::*;
pub use cors::*;
pub use csrf::*;
pub use embeddings::*;
pub use guardrails::*;
pub use headers::*;
pub use health::*;
pub use imagegen::*;
pub use llm::*;
pub use mcp::*;
pub use oauth::*;
pub use proxy::*;
pub use rate_limit::*;
pub use server::*;
pub use stt::*;
pub use telemetry::TelemetryConfig;
pub use tls::*;
pub use tts::*;

/// Top-level Synapse configuration
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Server configuration
    #[serde(default)]
    pub server: ServerConfig,
    /// LLM provider configuration
    #[serde(default)]
    pub llm: LlmConfig,
    /// MCP server configuration
    #[serde(default)]
    pub mcp: McpConfig,
    /// Embeddings provider configuration
    #[serde(default)]
    pub embeddings: EmbeddingsConfig,
    /// Image generation provider configuration
    #[serde(default)]
    pub imagegen: ImageGenConfig,
    /// STT provider configuration
    #[serde(default)]
    pub stt: SttConfig,
    /// TTS provider configuration
    #[serde(default)]
    pub tts: TtsConfig,
    /// Telemetry configuration
    #[serde(default)]
    pub telemetry: Option<TelemetryConfig>,
    /// Proxy configuration
    #[serde(default)]
    pub proxy: Option<ProxyConfig>,
    /// API key authentication configuration
    #[serde(default)]
    pub auth: Option<AuthConfig>,
    /// Billing and metering configuration
    #[serde(default)]
    pub billing: Option<BillingConfig>,
    /// Response cache configuration
    #[serde(default)]
    pub cache: Option<ResponseCacheConfig>,
    /// Content guardrails configuration
    #[serde(default)]
    pub guardrails: Option<GuardrailsConfig>,
}
