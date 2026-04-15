use indexmap::IndexMap;
use secrecy::SecretString;
use serde::Deserialize;

/// Top-level image generation configuration
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageGenConfig {
    /// Image generation provider configurations keyed by name
    #[serde(default)]
    pub providers: IndexMap<String, ImageGenProviderConfig>,
}

/// Configuration for a single image generation provider
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageGenProviderConfig {
    /// Provider type
    #[serde(rename = "type")]
    pub provider_type: ImageGenProviderType,
    /// API key
    #[serde(default)]
    pub api_key: Option<SecretString>,
    /// Base URL override
    #[serde(default)]
    pub base_url: Option<String>,
}

/// Supported image generation providers
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageGenProviderType {
    /// `OpenAI` image generation
    Openai,
}
