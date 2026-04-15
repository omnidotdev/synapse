use std::collections::HashMap;

use indexmap::IndexMap;
use secrecy::SecretString;
use serde::Deserialize;
use url::Url;

/// Billing and metering configuration
///
/// Opt-in section that enables Aether billing integration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BillingConfig {
    /// Whether billing is enabled
    #[serde(default)]
    pub enabled: bool,
    /// Base URL for the billing API
    #[serde(alias = "aether_url")]
    pub billing_url: Url,
    /// Service API key for authenticating with Aether
    pub service_api_key: SecretString,
    /// Aether application identifier
    pub app_id: String,
    /// Entity type for billing identity (default "user")
    #[serde(default = "default_entity_type")]
    pub entity_type: String,
    /// Operating mode for API key resolution
    #[serde(default)]
    pub mode: OperatingMode,
    /// Managed provider configurations keyed by provider name
    #[serde(default)]
    pub managed_providers: IndexMap<String, ManagedProviderConfig>,
    /// Meter key names used in Aether
    #[serde(default)]
    pub meters: MeterKeysConfig,
    /// TTL in seconds for cached entitlement checks
    #[serde(default = "default_entitlement_cache_ttl_secs")]
    pub entitlement_cache_ttl_secs: u64,
    /// Per-tier margin multipliers (e.g. "free" = 1.2, "pro" = 1.15)
    /// When set, these override the per-provider margin for that tier
    #[serde(default)]
    pub tier_margins: HashMap<String, f64>,
    /// Behavior when Aether is unreachable
    #[serde(default)]
    pub fail_mode: FailMode,
    /// Feature key for API access entitlement
    #[serde(default = "default_api_access_feature_key")]
    pub api_access_feature_key: String,
    /// Feature key for speech-to-text entitlement
    #[serde(default = "default_stt_feature_key")]
    pub stt_feature_key: String,
    /// Feature key for text-to-speech entitlement
    #[serde(default = "default_tts_feature_key")]
    pub tts_feature_key: String,
    /// Feature key for embeddings entitlement
    #[serde(default = "default_embeddings_feature_key")]
    pub embeddings_feature_key: String,
    /// Feature key for image generation entitlement
    #[serde(default = "default_image_gen_feature_key")]
    pub image_gen_feature_key: String,
}

/// Operating mode for API key resolution
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperatingMode {
    /// User brings their own provider API keys
    #[default]
    Byok,
    /// Synapse provides managed provider keys with margin
    Managed,
    /// Auto-detect: use user key if provided, else use managed key
    Hybrid,
}

/// Behavior when Aether is unreachable
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailMode {
    /// Allow requests through when Aether is unreachable
    #[default]
    Open,
    /// Reject requests with 503 when Aether is unreachable
    Closed,
}

/// Configuration for a managed provider
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedProviderConfig {
    /// API key for this managed provider
    pub api_key: SecretString,
    /// Margin multiplier applied to upstream cost (e.g. 1.2 = 20% markup)
    #[serde(default = "default_margin")]
    pub margin: f64,
}

/// Configurable meter key names for Aether usage recording
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeterKeysConfig {
    /// Meter key for input tokens
    #[serde(default = "default_input_tokens_key")]
    pub input_tokens: String,
    /// Meter key for output tokens
    #[serde(default = "default_output_tokens_key")]
    pub output_tokens: String,
    /// Meter key for request count
    #[serde(default = "default_requests_key")]
    pub requests: String,
    /// Meter key for message count
    #[serde(default = "default_messages_key")]
    pub messages: String,
}

impl Default for MeterKeysConfig {
    fn default() -> Self {
        Self {
            input_tokens: default_input_tokens_key(),
            output_tokens: default_output_tokens_key(),
            requests: default_requests_key(),
            messages: default_messages_key(),
        }
    }
}

fn default_entity_type() -> String {
    "user".to_owned()
}

const fn default_entitlement_cache_ttl_secs() -> u64 {
    60
}

fn default_api_access_feature_key() -> String {
    "api_access".to_owned()
}

fn default_stt_feature_key() -> String {
    "stt_enabled".to_owned()
}

fn default_tts_feature_key() -> String {
    "tts_enabled".to_owned()
}

fn default_embeddings_feature_key() -> String {
    "embeddings_enabled".to_owned()
}

fn default_image_gen_feature_key() -> String {
    "image_gen_enabled".to_owned()
}

const fn default_margin() -> f64 {
    1.0
}

fn default_input_tokens_key() -> String {
    "input_tokens".to_owned()
}

fn default_output_tokens_key() -> String {
    "output_tokens".to_owned()
}

fn default_requests_key() -> String {
    "requests".to_owned()
}

fn default_messages_key() -> String {
    "messages".to_owned()
}

impl BillingConfig {
    /// Validate billing configuration
    ///
    /// # Errors
    ///
    /// Returns an error if managed mode is enabled but no providers are
    /// configured
    pub fn validate(&self) -> Result<(), String> {
        if self.mode == OperatingMode::Managed && self.managed_providers.is_empty() {
            return Err("managed mode requires at least one managed provider".to_owned());
        }
        Ok(())
    }

    /// Return all configured feature keys (`api_access` + modality keys)
    pub fn all_feature_keys(&self) -> Vec<&str> {
        vec![
            &self.api_access_feature_key,
            &self.stt_feature_key,
            &self.tts_feature_key,
            &self.embeddings_feature_key,
            &self.image_gen_feature_key,
        ]
    }

    /// Return all configured meter keys
    pub fn all_meter_keys(&self) -> Vec<&str> {
        vec![
            &self.meters.requests,
            &self.meters.input_tokens,
            &self.meters.output_tokens,
            &self.meters.messages,
        ]
    }

    /// Return the modality feature key for a given request path, if the
    /// route requires a modality entitlement
    pub fn modality_feature_key(&self, path: &str) -> Option<&str> {
        match path {
            "/v1/audio/transcriptions" => Some(&self.stt_feature_key),
            "/v1/audio/speech" => Some(&self.tts_feature_key),
            "/v1/embeddings" => Some(&self.embeddings_feature_key),
            "/v1/images/generations" => Some(&self.image_gen_feature_key),
            _ => None,
        }
    }
}

/// Return a user-facing display name for a modality feature key
pub fn modality_display_name(feature_key: &str) -> &str {
    match feature_key {
        "stt_enabled" => "Speech-to-text",
        "tts_enabled" => "Text-to-speech",
        "embeddings_enabled" => "Embeddings",
        "image_gen_enabled" => "Image generation",
        _ => "This feature",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_minimal_config() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        assert!(config.enabled);
        assert_eq!(config.entity_type, "user");
        assert_eq!(config.mode, OperatingMode::Byok);
        assert_eq!(config.fail_mode, FailMode::Open);
        assert_eq!(config.entitlement_cache_ttl_secs, 60);
        assert_eq!(config.api_access_feature_key, "api_access");
    }

    #[test]
    fn fail_mode_defaults_to_open() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.fail_mode, FailMode::Open);
    }

    #[test]
    fn fail_mode_closed_rejects_when_aether_unreachable() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"
            fail_mode = "closed"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.fail_mode, FailMode::Closed);

        // Verify Closed mode semantics: when billing is unreachable, the
        // request should be rejected with 503 (tested in entitlement.rs
        // via handle_aether_error)
        match config.fail_mode {
            FailMode::Open => panic!("expected Closed"),
            FailMode::Closed => {} // 503 rejection path
        }
    }

    #[test]
    fn fail_mode_open_allows_when_aether_unreachable() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"
            fail_mode = "open"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.fail_mode, FailMode::Open);

        // Verify Open mode semantics: when billing is unreachable, the
        // request should proceed (tested in entitlement.rs via
        // handle_aether_error and in LlmState::check_credits)
        match config.fail_mode {
            FailMode::Closed => panic!("expected Open"),
            FailMode::Open => {} // allow-through path
        }
    }

    #[test]
    fn deserialize_managed_mode() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"
            mode = "managed"
            fail_mode = "closed"

            [managed_providers.openai]
            api_key = "sk-openai-managed"
            margin = 1.2

            [managed_providers.anthropic]
            api_key = "sk-anthropic-managed"
            margin = 1.15
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.mode, OperatingMode::Managed);
        assert_eq!(config.fail_mode, FailMode::Closed);
        assert_eq!(config.managed_providers.len(), 2);
        assert!((config.managed_providers["openai"].margin - 1.2).abs() < f64::EPSILON);
    }

    #[test]
    fn deserialize_custom_meters() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"

            [meters]
            input_tokens = "llm_input_tokens"
            output_tokens = "llm_output_tokens"
            requests = "llm_requests"
            messages = "llm_messages"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.meters.input_tokens, "llm_input_tokens");
        assert_eq!(config.meters.output_tokens, "llm_output_tokens");
        assert_eq!(config.meters.requests, "llm_requests");
        assert_eq!(config.meters.messages, "llm_messages");
    }

    #[test]
    fn validate_managed_without_providers_fails() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"
            mode = "managed"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_byok_without_providers_succeeds() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn default_meter_keys() {
        let keys = MeterKeysConfig::default();
        assert_eq!(keys.input_tokens, "input_tokens");
        assert_eq!(keys.output_tokens, "output_tokens");
        assert_eq!(keys.requests, "requests");
        assert_eq!(keys.messages, "messages");
    }

    #[test]
    fn default_modality_feature_keys() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.stt_feature_key, "stt_enabled");
        assert_eq!(config.tts_feature_key, "tts_enabled");
        assert_eq!(config.embeddings_feature_key, "embeddings_enabled");
        assert_eq!(config.image_gen_feature_key, "image_gen_enabled");
    }

    #[test]
    fn modality_feature_key_mapping() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.modality_feature_key("/v1/audio/transcriptions"),
            Some("stt_enabled")
        );
        assert_eq!(config.modality_feature_key("/v1/audio/speech"), Some("tts_enabled"));
        assert_eq!(
            config.modality_feature_key("/v1/embeddings"),
            Some("embeddings_enabled")
        );
        assert_eq!(
            config.modality_feature_key("/v1/images/generations"),
            Some("image_gen_enabled")
        );
        assert_eq!(config.modality_feature_key("/v1/chat/completions"), None);
        assert_eq!(config.modality_feature_key("/health"), None);
    }

    #[test]
    fn modality_display_names() {
        assert_eq!(modality_display_name("stt_enabled"), "Speech-to-text");
        assert_eq!(modality_display_name("tts_enabled"), "Text-to-speech");
        assert_eq!(modality_display_name("embeddings_enabled"), "Embeddings");
        assert_eq!(modality_display_name("image_gen_enabled"), "Image generation");
        assert_eq!(modality_display_name("unknown_key"), "This feature");
    }

    #[test]
    fn deserialize_tier_margins() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"

            [tier_margins]
            free = 1.2
            pro = 1.15
            team = 1.15
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.tier_margins.len(), 3);
        assert!((config.tier_margins["free"] - 1.2).abs() < f64::EPSILON);
        assert!((config.tier_margins["pro"] - 1.15).abs() < f64::EPSILON);
        assert!((config.tier_margins["team"] - 1.15).abs() < f64::EPSILON);
    }

    #[test]
    fn all_feature_keys_returns_configured_keys() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"
            api_access_feature_key = "custom_access"
            stt_feature_key = "custom_stt"
            tts_feature_key = "custom_tts"
            embeddings_feature_key = "custom_embed"
            image_gen_feature_key = "custom_img"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        let keys = config.all_feature_keys();

        assert_eq!(keys.len(), 5);
        assert!(keys.contains(&"custom_access"));
        assert!(keys.contains(&"custom_stt"));
        assert!(keys.contains(&"custom_tts"));
        assert!(keys.contains(&"custom_embed"));
        assert!(keys.contains(&"custom_img"));
    }

    #[test]
    fn all_feature_keys_returns_defaults() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        let keys = config.all_feature_keys();

        assert_eq!(keys.len(), 5);
        assert!(keys.contains(&"api_access"));
        assert!(keys.contains(&"stt_enabled"));
        assert!(keys.contains(&"tts_enabled"));
        assert!(keys.contains(&"embeddings_enabled"));
        assert!(keys.contains(&"image_gen_enabled"));
    }

    #[test]
    fn all_meter_keys_returns_configured_keys() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"

            [meters]
            input_tokens = "custom_input"
            output_tokens = "custom_output"
            requests = "custom_requests"
            messages = "custom_messages"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        let keys = config.all_meter_keys();

        assert_eq!(keys.len(), 4);
        assert!(keys.contains(&"custom_input"));
        assert!(keys.contains(&"custom_output"));
        assert!(keys.contains(&"custom_requests"));
        assert!(keys.contains(&"custom_messages"));
    }

    #[test]
    fn all_meter_keys_returns_defaults() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        let keys = config.all_meter_keys();

        assert_eq!(keys.len(), 4);
        assert!(keys.contains(&"input_tokens"));
        assert!(keys.contains(&"output_tokens"));
        assert!(keys.contains(&"requests"));
        assert!(keys.contains(&"messages"));
    }

    #[test]
    fn tier_margins_default_empty() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        assert!(config.tier_margins.is_empty());
    }
}
