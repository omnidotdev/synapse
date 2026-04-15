use indexmap::IndexMap;
use secrecy::SecretString;
use serde::Deserialize;

/// Top-level embeddings configuration
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingsConfig {
    /// Embeddings provider configurations keyed by name
    #[serde(default)]
    pub providers: IndexMap<String, EmbeddingsProviderConfig>,
}

/// Configuration for a single embeddings provider
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingsProviderConfig {
    /// Provider type
    #[serde(rename = "type")]
    pub provider_type: EmbeddingsProviderType,
    /// API key
    #[serde(default)]
    pub api_key: Option<SecretString>,
    /// Base URL override
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Supported embeddings providers
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingsProviderType {
    /// `OpenAI` embeddings
    Openai,
}
