use serde::Deserialize;

/// Proxy configuration
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    /// Anthropic proxy configuration
    #[serde(default)]
    pub anthropic: Option<AnthropicProxyConfig>,
}

/// Anthropic-specific proxy configuration
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicProxyConfig {
    /// Enable the Anthropic proxy endpoint
    #[serde(default)]
    pub enabled: bool,
    /// Path prefix for the proxy endpoint
    #[serde(default = "default_anthropic_path")]
    pub path: String,
}

fn default_anthropic_path() -> String {
    "/anthropic".to_string()
}
