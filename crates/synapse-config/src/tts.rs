use indexmap::IndexMap;
use secrecy::SecretString;
use serde::Deserialize;

/// Top-level TTS configuration
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TtsConfig {
    /// TTS provider configurations keyed by name
    #[serde(default)]
    pub providers: IndexMap<String, TtsProviderConfig>,
}

/// Configuration for a single TTS provider
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TtsProviderConfig {
    /// Provider type
    #[serde(rename = "type")]
    pub provider_type: TtsProviderType,
    /// API key
    #[serde(default)]
    pub api_key: Option<SecretString>,
    /// Base URL override
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Supported TTS providers
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TtsProviderType {
    /// `OpenAI` TTS
    OpenaiTts,
    /// `ElevenLabs`
    Elevenlabs,
}
