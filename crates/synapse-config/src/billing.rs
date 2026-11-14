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
    /// When set, these override the per-provider margin for that tier.
    /// They apply only to this gateway's own product (`app_id`), since the
    /// keys are this product's plan names
    #[serde(default)]
    pub tier_margins: HashMap<String, f64>,
    /// Per-product margin multipliers for product-scoped keys (app id to
    /// multiplier, e.g. "facet" = 2.0). Product-scoped traffic never uses
    /// `tier_margins` or provider margins
    #[serde(default)]
    pub product_margins: HashMap<String, f64>,
    /// Margin for product-scoped traffic whose product has no entry in
    /// `product_margins` (the fleet retail markup)
    #[serde(default = "default_product_margin")]
    pub default_product_margin: f64,
    /// Products whose managed AI is included in their plan tier, bounded by a
    /// monthly fair-use allowance (Posture C), as app id to the Aether usage
    /// meter holding that allowance in credits (e.g. `myfi = "ai_credits"`).
    /// Their AI spend is checked against and recorded on that meter, never
    /// the pooled credit wallet
    #[serde(default)]
    pub included_allowance_products: HashMap<String, String>,
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
    /// Allow requests through under an emergency token ceiling when Aether is
    /// unreachable. Preserves availability during an Aether outage while
    /// bounding per-request spend (so an outage cannot become unmetered spend),
    /// and is the safe default. BYOK requests are unaffected (no metering).
    #[default]
    Degraded,
    /// Allow requests through on full (free-tier) limits when Aether is
    /// unreachable. Max availability, but an outage means unbounded free-tier
    /// spend across the fleet.
    Open,
    /// Reject requests with 503 when Aether is unreachable. Max cost safety, but
    /// an outage takes AI down.
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

const fn default_product_margin() -> f64 {
    2.0
}

/// Reject a margin that would sell managed usage below upstream cost
fn check_margin(label: &str, margin: f64) -> Result<(), String> {
    if margin.is_finite() && margin >= 1.0 {
        Ok(())
    } else {
        Err(format!(
            "{label} must be a finite multiplier of at least 1.0, got {margin}"
        ))
    }
}

/// Whether a meter key is safe as an Aether URL path segment
fn is_meter_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
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
    /// configured, if any margin would sell managed usage below cost, or if an
    /// included-allowance product is the gateway's own or names a malformed
    /// meter
    pub fn validate(&self) -> Result<(), String> {
        if self.mode == OperatingMode::Managed && self.managed_providers.is_empty() {
            return Err("managed mode requires at least one managed provider".to_owned());
        }
        check_margin("billing.default_product_margin", self.default_product_margin)?;
        for (product, margin) in &self.product_margins {
            check_margin(&format!("billing.product_margins.{product}"), *margin)?;
        }
        for (tier, margin) in &self.tier_margins {
            check_margin(&format!("billing.tier_margins.{tier}"), *margin)?;
        }
        for (provider, config) in &self.managed_providers {
            check_margin(&format!("billing.managed_providers.{provider}.margin"), config.margin)?;
        }
        for (product, meter) in &self.included_allowance_products {
            if product == &self.app_id {
                return Err(format!(
                    "billing.included_allowance_products.{product}: the gateway's own product bills its plans \
                     through the credit wallet"
                ));
            }
            if !is_meter_key(meter) {
                return Err(format!(
                    "billing.included_allowance_products.{product} must be a meter key of lowercase letters, \
                     digits, and underscores, got {meter:?}"
                ));
            }
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
        assert_eq!(config.fail_mode, FailMode::Degraded);
        assert_eq!(config.entitlement_cache_ttl_secs, 60);
        assert_eq!(config.api_access_feature_key, "api_access");
    }

    #[test]
    fn fail_mode_defaults_to_degraded() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.fail_mode, FailMode::Degraded);
    }

    #[test]
    fn fail_mode_degraded_parses() {
        let toml = r#"
            enabled = true
            billing_url = "https://billing.omni.dev/"
            service_api_key = "sk-test-123"
            app_id = "synapse"
            fail_mode = "degraded"
        "#;

        let config: BillingConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.fail_mode, FailMode::Degraded);
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
            FailMode::Open | FailMode::Degraded => panic!("expected Closed"),
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
            FailMode::Closed | FailMode::Degraded => panic!("expected Open"),
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

    const BASE: &str = r#"
        enabled = true
        billing_url = "https://billing.omni.dev/"
        service_api_key = "sk-test-123"
        app_id = "synapse"
    "#;

    fn parse(extra: &str) -> BillingConfig {
        toml::from_str(&format!("{BASE}\n{extra}")).unwrap()
    }

    #[test]
    fn product_margins_default_to_retail_markup() {
        let config = parse("");
        assert!(config.product_margins.is_empty());
        assert!((config.default_product_margin - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn deserialize_product_margins() {
        let config = parse(
            r"
            default_product_margin = 1.8

            [product_margins]
            facet = 2.0
            keystone = 2.5
            ",
        );
        assert!((config.product_margins["facet"] - 2.0).abs() < f64::EPSILON);
        assert!((config.product_margins["keystone"] - 2.5).abs() < f64::EPSILON);
        assert!((config.default_product_margin - 1.8).abs() < f64::EPSILON);
    }

    #[test]
    fn validate_rejects_any_margin_below_cost() {
        let below_cost = [
            "default_product_margin = 0.99",
            "[product_margins]\nfacet = 0.5",
            "[tier_margins]\npro = 0.9",
            "[managed_providers.openai]\napi_key = \"sk\"\nmargin = 0.8",
            "[product_margins]\nfacet = nan",
        ];
        for extra in below_cost {
            assert!(parse(extra).validate().is_err(), "accepted: {extra}");
        }
    }

    #[test]
    fn included_allowance_products_default_to_none() {
        assert!(parse("").included_allowance_products.is_empty());
    }

    #[test]
    fn deserialize_included_allowance_products() {
        let config = parse(
            r#"
            [included_allowance_products]
            myfi = "ai_credits"
            "#,
        );
        assert_eq!(config.included_allowance_products["myfi"], "ai_credits");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_a_malformed_allowance_meter() {
        for meter in ["", "ai credits", "ai/credits", "AI_CREDITS", "../x"] {
            let config = parse(&format!("[included_allowance_products]\nmyfi = {meter:?}"));
            assert!(config.validate().is_err(), "accepted meter {meter:?}");
        }
    }

    #[test]
    fn validate_rejects_the_gateway_own_product_as_an_allowance_product() {
        // The gateway's own plans bill through the credit wallet
        let config = parse("[included_allowance_products]\nsynapse = \"ai_credits\"");
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_accepts_margins_at_or_above_cost() {
        let config = parse(
            r#"
            [tier_margins]
            free = 1.15

            [product_margins]
            facet = 2.0

            [managed_providers.openai]
            api_key = "sk"
            margin = 1.0
            "#,
        );
        assert!(config.validate().is_ok());
    }
}
