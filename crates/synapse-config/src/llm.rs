use std::collections::HashMap;

use indexmap::IndexMap;
use secrecy::SecretString;
use serde::Deserialize;
use url::Url;

use crate::headers::HeaderRuleConfig;

/// Top-level LLM configuration
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    /// LLM provider configurations keyed by name
    #[serde(default)]
    pub providers: IndexMap<String, LlmProviderConfig>,
    /// Failover configuration for automatic provider fallback
    #[serde(default)]
    pub failover: FailoverConfig,
    /// Smart model routing configuration
    #[serde(default)]
    pub routing: RoutingConfig,
}

/// Configuration for a single LLM provider
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmProviderConfig {
    /// Provider protocol type
    #[serde(rename = "type")]
    pub provider_type: LlmProviderType,
    /// API key for authentication
    #[serde(default)]
    pub api_key: Option<SecretString>,
    /// Base URL override
    #[serde(default)]
    pub base_url: Option<Url>,
    /// Model configuration
    #[serde(default)]
    pub models: ModelConfig,
    /// Header rules for this provider
    #[serde(default)]
    pub headers: Vec<HeaderRuleConfig>,
    /// Forward the client's bearer token to the provider
    #[serde(default)]
    pub forward_authorization: bool,
    /// Rate limit for this provider (requests per window)
    #[serde(default)]
    pub rate_limit: Option<ProviderRateLimit>,
}

/// Supported LLM provider protocols
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProviderType {
    /// OpenAI-compatible API
    Openai,
    /// Anthropic Messages API
    Anthropic,
    /// Google Generative Language API
    Google,
    /// AWS Bedrock
    Bedrock(BedrockConfig),
}

/// AWS Bedrock-specific configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BedrockConfig {
    /// AWS region
    pub region: String,
    /// Access key ID (optional, uses default credential chain if absent)
    #[serde(default)]
    pub access_key_id: Option<SecretString>,
    /// Secret access key
    #[serde(default)]
    pub secret_access_key: Option<SecretString>,
}

/// Model configuration for a provider
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    /// Include models matching these patterns (regex)
    #[serde(default)]
    pub include: Vec<String>,
    /// Exclude models matching these patterns (regex)
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Per-model overrides
    #[serde(default)]
    pub overrides: HashMap<String, ModelOverride>,
}

/// Per-model configuration overrides
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelOverride {
    /// Custom display name
    #[serde(default)]
    pub alias: Option<String>,
    /// Rate limit override for this model
    #[serde(default)]
    pub rate_limit: Option<ProviderRateLimit>,
}

/// Rate limit configuration for a provider or model
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRateLimit {
    /// Maximum requests per window
    pub requests: u32,
    /// Window duration (e.g. "1m", "1h")
    pub window: String,
}

// -- Failover configuration --

/// Configuration for automatic provider failover
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailoverConfig {
    /// Whether failover is enabled
    #[serde(default)]
    pub enabled: bool,
    /// Maximum number of providers to try (including the primary)
    #[serde(default = "default_max_attempts")]
    pub max_attempts: usize,
    /// Groups of equivalent models that can substitute for each other
    #[serde(default)]
    pub equivalence_groups: Vec<EquivalenceGroup>,
    /// Circuit breaker configuration for provider health tracking
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_attempts: default_max_attempts(),
            equivalence_groups: Vec::new(),
            circuit_breaker: CircuitBreakerConfig::default(),
        }
    }
}

const fn default_max_attempts() -> usize {
    2
}

/// A group of models that can substitute for each other during failover
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EquivalenceGroup {
    /// Human-readable group name (e.g. "frontier", "fast")
    pub name: String,
    /// Model identifiers in "provider/model" format
    pub models: Vec<String>,
}

/// Circuit breaker configuration for provider health tracking
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CircuitBreakerConfig {
    /// Number of errors within the window to trip the breaker
    #[serde(default = "default_error_threshold")]
    pub error_threshold: u32,
    /// Sliding window duration in seconds
    #[serde(default = "default_window_seconds")]
    pub window_seconds: u64,
    /// Seconds to wait before probing a tripped provider
    #[serde(default = "default_recovery_seconds")]
    pub recovery_seconds: u64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            error_threshold: default_error_threshold(),
            window_seconds: default_window_seconds(),
            recovery_seconds: default_recovery_seconds(),
        }
    }
}

const fn default_error_threshold() -> u32 {
    5
}

const fn default_window_seconds() -> u64 {
    60
}

const fn default_recovery_seconds() -> u64 {
    30
}

// -- Routing configuration --

/// Smart model routing configuration
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingConfig {
    /// Whether smart routing is enabled
    #[serde(default)]
    pub enabled: bool,
    /// Routing strategy to use
    #[serde(default)]
    pub strategy: RoutingStrategy,
    /// Model profiles for cost/quality scoring
    #[serde(default)]
    pub models: Vec<ModelProfileConfig>,
    /// Threshold strategy configuration
    #[serde(default)]
    pub threshold: ThresholdConfig,
    /// Cost-constrained strategy configuration
    #[serde(default)]
    pub cost: CostConfig,
    /// Cascade strategy configuration
    #[serde(default)]
    pub cascade: CascadeConfig,
    /// Score strategy configuration
    #[serde(default)]
    pub score: ScoreConfig,
}

/// Available routing strategies
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingStrategy {
    /// Route by query complexity: simple queries to cheap models, complex to frontier
    #[default]
    Threshold,
    /// Maximize quality within a cost budget
    Cost,
    /// Try cheap model first, escalate if low confidence
    Cascade,
    /// Multi-objective weighted scoring (quality + cost + latency)
    Score,
    /// User-registered custom strategy referenced by name
    Custom(String),
}

/// Configuration for a model profile used in smart routing
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfileConfig {
    /// Provider name (key in providers config)
    pub provider: String,
    /// Model identifier
    pub model: String,
    /// Context window size in tokens
    #[serde(default)]
    pub context_window: u32,
    /// Cost per million input tokens (USD)
    #[serde(default)]
    pub input_per_mtok: f64,
    /// Cost per million output tokens (USD)
    #[serde(default)]
    pub output_per_mtok: f64,
    /// Quality score (0.0 to 1.0), seeded from benchmarks
    #[serde(default)]
    pub quality: f64,
    /// Model capabilities
    #[serde(default)]
    pub capabilities: ModelCapabilities,
}

/// Capabilities a model may support
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCapabilities {
    /// Whether the model supports tool/function calling
    #[serde(default)]
    pub tool_calling: bool,
    /// Whether the model supports vision/image inputs
    #[serde(default)]
    pub vision: bool,
    /// Whether the model handles long contexts well
    #[serde(default)]
    pub long_context: bool,
}

/// Configuration for threshold-based routing
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThresholdConfig {
    /// Model to use for low-complexity queries ("provider/model")
    #[serde(default)]
    pub low_complexity_model: Option<String>,
    /// Model to use for high-complexity queries ("provider/model")
    #[serde(default)]
    pub high_complexity_model: Option<String>,
    /// Minimum quality score to consider a model (0.0 to 1.0)
    #[serde(default = "default_quality_floor")]
    pub quality_floor: f64,
}

impl Default for ThresholdConfig {
    fn default() -> Self {
        Self {
            low_complexity_model: None,
            high_complexity_model: None,
            quality_floor: default_quality_floor(),
        }
    }
}

const fn default_quality_floor() -> f64 {
    0.7
}

/// Configuration for cost-constrained routing
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostConfig {
    /// Maximum cost per request in USD
    #[serde(default)]
    pub max_cost_per_request: Option<f64>,
}

/// Configuration for cascade routing
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CascadeConfig {
    /// Initial cheap model to try first ("provider/model")
    #[serde(default)]
    pub initial_model: Option<String>,
    /// Stronger model to escalate to ("provider/model")
    #[serde(default)]
    pub escalation_model: Option<String>,
    /// Confidence threshold below which to escalate (0.0 to 1.0)
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,
    /// Maximum bytes to buffer during streaming cascade before committing
    #[serde(default = "default_max_buffer_bytes")]
    pub max_buffer_bytes: usize,
    /// Seconds to wait for buffered response before committing
    #[serde(default = "default_buffer_timeout_secs")]
    pub buffer_timeout_secs: u64,
}

impl Default for CascadeConfig {
    fn default() -> Self {
        Self {
            initial_model: None,
            escalation_model: None,
            confidence_threshold: default_confidence_threshold(),
            max_buffer_bytes: default_max_buffer_bytes(),
            buffer_timeout_secs: default_buffer_timeout_secs(),
        }
    }
}

const fn default_confidence_threshold() -> f64 {
    0.5
}

const fn default_max_buffer_bytes() -> usize {
    32 * 1024 // 32 KB
}

const fn default_buffer_timeout_secs() -> u64 {
    10
}

/// Configuration for multi-objective score routing
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoreConfig {
    /// Weight for quality component (0.0 to 1.0)
    #[serde(default = "default_weight_quality")]
    pub weight_quality: f64,
    /// Weight for cost component (0.0 to 1.0)
    #[serde(default = "default_weight_cost")]
    pub weight_cost: f64,
    /// Weight for latency component (0.0 to 1.0)
    #[serde(default = "default_weight_latency")]
    pub weight_latency: f64,
    /// Error rate penalty multiplier
    #[serde(default = "default_error_penalty")]
    pub error_penalty: f64,
    /// Minimum samples before feedback influences scoring
    #[serde(default = "default_min_samples")]
    pub min_samples: usize,
}

impl Default for ScoreConfig {
    fn default() -> Self {
        Self {
            weight_quality: default_weight_quality(),
            weight_cost: default_weight_cost(),
            weight_latency: default_weight_latency(),
            error_penalty: default_error_penalty(),
            min_samples: default_min_samples(),
        }
    }
}

const fn default_weight_quality() -> f64 {
    0.5
}

const fn default_weight_cost() -> f64 {
    0.3
}

const fn default_weight_latency() -> f64 {
    0.2
}

const fn default_error_penalty() -> f64 {
    1.0
}

const fn default_min_samples() -> usize {
    10
}
