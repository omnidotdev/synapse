use indexmap::IndexMap;
use secrecy::SecretString;
use serde::Deserialize;

/// Top-level STT configuration
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SttConfig {
    /// STT provider configurations keyed by name
    #[serde(default)]
    pub providers: IndexMap<String, SttProviderConfig>,
}

/// Configuration for a single STT provider
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SttProviderConfig {
    /// Provider type
    #[serde(rename = "type")]
    pub provider_type: SttProviderType,
    /// API key
    #[serde(default)]
    pub api_key: Option<SecretString>,
    /// Base URL override
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Supported STT providers
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SttProviderType {
    /// `OpenAI` Whisper
    Whisper,
    /// Deepgram
    Deepgram,
}
