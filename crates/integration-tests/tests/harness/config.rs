#![allow(dead_code)]
//! Programmatic configuration builder for integration tests

use std::net::SocketAddr;

use secrecy::SecretString;
use synapse_config::{
    CircuitBreakerConfig, Config, CorsConfig, CsrfConfig, EmbeddingsConfig, EmbeddingsProviderConfig,
    EmbeddingsProviderType, EquivalenceGroup, FailoverConfig, HealthConfig, ImageGenConfig, ImageGenProviderConfig,
    ImageGenProviderType, LlmConfig, LlmProviderConfig, LlmProviderType, McpConfig, ModelConfig, RateLimitConfig,
    ServerConfig, SttConfig, TtsConfig,
};

/// Builder for constructing test configurations
pub struct ConfigBuilder {
    config: Config,
}

impl ConfigBuilder {
    /// Create a new builder with minimal defaults
    pub fn new() -> Self {
        Self {
            config: Config {
                server: ServerConfig {
                    listen_address: Some(SocketAddr::from(([127, 0, 0, 1], 0))),
                    health: HealthConfig {
                        enabled: true,
                        ..HealthConfig::default()
                    },
                    ..ServerConfig::default()
                },
                llm: LlmConfig::default(),
                mcp: McpConfig::default(),
                embeddings: EmbeddingsConfig::default(),
                imagegen: ImageGenConfig::default(),
                stt: SttConfig::default(),
                tts: TtsConfig::default(),
                telemetry: None,
                proxy: None,
                billing: None,
                auth: None,
                cache: None,
                guardrails: None,
            },
        }
    }

    /// Add an OpenAI-compatible provider pointed at a mock backend
    pub fn with_openai_provider(mut self, name: &str, base_url: &str) -> Self {
        self.config.llm.providers.insert(
            name.to_owned(),
            LlmProviderConfig {
                provider_type: LlmProviderType::Openai,
                api_key: Some(SecretString::from("test-key")),
                base_url: Some(base_url.parse().expect("valid URL")),
                models: ModelConfig::default(),
                headers: Vec::new(),
                forward_authorization: false,
                rate_limit: None,
            },
        );
        self
    }

    /// Add an OpenAI-compatible embeddings provider pointed at a mock backend
    pub fn with_embeddings_provider(mut self, name: &str, base_url: &str) -> Self {
        self.config.embeddings.providers.insert(
            name.to_owned(),
            EmbeddingsProviderConfig {
                provider_type: EmbeddingsProviderType::Openai,
                api_key: Some(SecretString::from("test-key")),
                base_url: Some(base_url.to_owned()),
            },
        );
        self
    }

    /// Add an OpenAI-compatible image generation provider pointed at a mock backend
    pub fn with_imagegen_provider(mut self, name: &str, base_url: &str) -> Self {
        self.config.imagegen.providers.insert(
            name.to_owned(),
            ImageGenProviderConfig {
                provider_type: ImageGenProviderType::Openai,
                api_key: Some(SecretString::from("test-key")),
                base_url: Some(base_url.to_owned()),
            },
        );
        self
    }

    /// Set CORS configuration
    pub fn with_cors(mut self, config: CorsConfig) -> Self {
        self.config.server.cors = Some(config);
        self
    }

    /// Set CSRF configuration
    pub fn with_csrf(mut self, config: CsrfConfig) -> Self {
        self.config.server.csrf = Some(config);
        self
    }

    /// Set rate limit configuration
    pub fn with_rate_limit(mut self, config: RateLimitConfig) -> Self {
        self.config.server.rate_limit = Some(config);
        self
    }

    /// Disable health endpoint
    pub fn without_health(mut self) -> Self {
        self.config.server.health.enabled = false;
        self
    }

    /// Enable failover with the given equivalence groups
    pub fn with_failover(mut self, groups: Vec<EquivalenceGroup>) -> Self {
        self.config.llm.failover = FailoverConfig {
            enabled: true,
            max_attempts: 3,
            equivalence_groups: groups,
            circuit_breaker: CircuitBreakerConfig {
                error_threshold: 5,
                window_seconds: 60,
                recovery_seconds: 30,
            },
        };
        self
    }

    /// Build the final config
    pub fn build(self) -> Config {
        self.config
    }
}
