//! Model registry with cost, quality, and capability profiles
//!
//! Config-driven profiles analogous to `LLMRouter`'s `llm_data` JSON
//! but sourced from TOML configuration.

use synapse_config::ModelProfileConfig;

use crate::analysis::RequiredCapabilities;

/// Runtime model profile with observed metrics
#[derive(Debug, Clone)]
pub struct ModelProfile {
    /// Provider name
    pub provider: String,
    /// Model identifier
    pub model: String,
    /// Context window in tokens
    pub context_window: u32,
    /// Cost per million input tokens (USD)
    pub input_per_mtok: f64,
    /// Cost per million output tokens (USD)
    pub output_per_mtok: f64,
    /// Quality score (0.0 to 1.0)
    pub quality: f64,
    /// Whether the model supports tool calling
    pub tool_calling: bool,
    /// Whether the model supports vision
    pub vision: bool,
    /// Whether the model handles long contexts well
    pub long_context: bool,
    /// Observed p50 latency in milliseconds (updated at runtime)
    pub observed_latency_p50_ms: Option<f64>,
}

impl ModelProfile {
    /// Canonical identifier in "provider/model" format
    pub fn id(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }

    /// Estimate the cost of a request with the given token counts
    pub fn estimate_cost(&self, input_tokens: usize, output_tokens: usize) -> f64 {
        let input_cost = (input_tokens as f64 / 1_000_000.0) * self.input_per_mtok;
        let output_cost = (output_tokens as f64 / 1_000_000.0) * self.output_per_mtok;
        input_cost + output_cost
    }
}

/// Registry of all available model profiles
#[derive(Debug)]
pub struct ModelRegistry {
    profiles: Vec<ModelProfile>,
}

impl ModelRegistry {
    /// Build a registry from configuration
    pub fn from_config(configs: &[ModelProfileConfig]) -> Self {
        let profiles = configs
            .iter()
            .map(|c| ModelProfile {
                provider: c.provider.clone(),
                model: c.model.clone(),
                context_window: c.context_window,
                input_per_mtok: c.input_per_mtok,
                output_per_mtok: c.output_per_mtok,
                quality: c.quality,
                tool_calling: c.capabilities.tool_calling,
                vision: c.capabilities.vision,
                long_context: c.capabilities.long_context,
                observed_latency_p50_ms: None,
            })
            .collect();

        Self { profiles }
    }

    /// Get all profiles
    pub fn profiles(&self) -> &[ModelProfile] {
        &self.profiles
    }

    /// Find a profile by provider/model pair
    pub fn find(&self, provider: &str, model: &str) -> Option<&ModelProfile> {
        self.profiles
            .iter()
            .find(|p| p.provider == provider && p.model == model)
    }

    /// Get profiles sorted by quality (highest first)
    pub fn by_quality(&self) -> Vec<&ModelProfile> {
        let mut sorted: Vec<&ModelProfile> = self.profiles.iter().collect();
        sorted.sort_by(|a, b| b.quality.partial_cmp(&a.quality).unwrap_or(std::cmp::Ordering::Equal));
        sorted
    }

    /// Get profiles sorted by cost (cheapest first, by input cost)
    pub fn by_cost(&self) -> Vec<&ModelProfile> {
        let mut sorted: Vec<&ModelProfile> = self.profiles.iter().collect();
        sorted.sort_by(|a, b| {
            a.input_per_mtok
                .partial_cmp(&b.input_per_mtok)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        sorted
    }

    /// Get the cheapest model above a quality floor
    pub fn cheapest_above_quality(&self, quality_floor: f64) -> Option<&ModelProfile> {
        self.by_cost().into_iter().find(|p| p.quality >= quality_floor)
    }

    /// Get the highest quality model
    pub fn best_quality(&self) -> Option<&ModelProfile> {
        self.by_quality().into_iter().next()
    }

    /// Return a new registry containing only models that satisfy the required capabilities
    #[must_use]
    pub fn filtered(&self, required: &RequiredCapabilities) -> Self {
        let profiles = self
            .profiles
            .iter()
            .filter(|p| {
                (!required.tool_calling || p.tool_calling)
                    && (!required.vision || p.vision)
                    && (!required.long_context || p.long_context)
            })
            .cloned()
            .collect();

        Self { profiles }
    }

    /// Update observed latency for a model
    pub fn update_latency(&mut self, provider: &str, model: &str, latency_p50_ms: f64) {
        if let Some(profile) = self
            .profiles
            .iter_mut()
            .find(|p| p.provider == provider && p.model == model)
        {
            profile.observed_latency_p50_ms = Some(latency_p50_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use synapse_config::{ModelCapabilities, ModelProfileConfig};

    fn test_profiles() -> Vec<ModelProfileConfig> {
        vec![
            ModelProfileConfig {
                provider: "anthropic".to_owned(),
                model: "claude-sonnet-4-20250514".to_owned(),
                context_window: 200_000,
                input_per_mtok: 3.0,
                output_per_mtok: 15.0,
                quality: 0.92,
                capabilities: ModelCapabilities {
                    tool_calling: true,
                    vision: true,
                    long_context: true,
                },
            },
            ModelProfileConfig {
                provider: "openai".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                context_window: 128_000,
                input_per_mtok: 0.15,
                output_per_mtok: 0.60,
                quality: 0.78,
                capabilities: ModelCapabilities {
                    tool_calling: true,
                    vision: true,
                    long_context: false,
                },
            },
        ]
    }

    #[test]
    fn cheapest_above_quality() {
        let registry = ModelRegistry::from_config(&test_profiles());
        let cheapest = registry.cheapest_above_quality(0.7).unwrap();
        assert_eq!(cheapest.model, "gpt-4o-mini");
    }

    #[test]
    fn best_quality() {
        let registry = ModelRegistry::from_config(&test_profiles());
        let best = registry.best_quality().unwrap();
        assert_eq!(best.model, "claude-sonnet-4-20250514");
    }

    #[test]
    fn estimate_cost() {
        let registry = ModelRegistry::from_config(&test_profiles());
        let profile = registry.find("openai", "gpt-4o-mini").unwrap();
        let cost = profile.estimate_cost(1_000_000, 500_000);
        // 1M * 0.15/1M + 0.5M * 0.60/1M = 0.15 + 0.30 = 0.45
        assert!((cost - 0.45).abs() < 0.001);
    }

    #[test]
    fn filtered_by_long_context() {
        let registry = ModelRegistry::from_config(&test_profiles());
        let caps = RequiredCapabilities {
            long_context: true,
            ..RequiredCapabilities::default()
        };
        let filtered = registry.filtered(&caps);
        assert_eq!(filtered.profiles().len(), 1);
        assert_eq!(filtered.profiles()[0].model, "claude-sonnet-4-20250514");
    }

    #[test]
    fn filtered_by_vision() {
        let registry = ModelRegistry::from_config(&test_profiles());
        // Both models support vision, so both should pass
        let caps = RequiredCapabilities {
            vision: true,
            ..RequiredCapabilities::default()
        };
        let filtered = registry.filtered(&caps);
        assert_eq!(filtered.profiles().len(), 2);
    }

    #[test]
    fn filtered_no_requirements_keeps_all() {
        let registry = ModelRegistry::from_config(&test_profiles());
        let caps = RequiredCapabilities::default();
        let filtered = registry.filtered(&caps);
        assert_eq!(filtered.profiles().len(), 2);
    }

    #[test]
    fn filtered_no_match() {
        // Add a model that lacks tool calling
        let profiles = vec![ModelProfileConfig {
            provider: "local".to_owned(),
            model: "tiny".to_owned(),
            context_window: 4096,
            input_per_mtok: 0.0,
            output_per_mtok: 0.0,
            quality: 0.5,
            capabilities: ModelCapabilities {
                tool_calling: false,
                vision: false,
                long_context: false,
            },
        }];
        let registry = ModelRegistry::from_config(&profiles);
        let caps = RequiredCapabilities {
            tool_calling: true,
            ..RequiredCapabilities::default()
        };
        let filtered = registry.filtered(&caps);
        assert!(filtered.profiles().is_empty());
    }
}
