//! Routing strategy implementations

use std::collections::HashMap;

use synapse_config::RoutingConfig;

use crate::RoutingDecision;
use crate::analysis::QueryProfile;
use crate::error::RoutingError;
use crate::feedback::FeedbackTracker;
use crate::registry::ModelRegistry;

pub mod cascade;
pub mod cost;
pub mod failover;
pub mod onnx;
pub mod score;
pub mod threshold;

/// Trait for routing strategy implementations
#[allow(clippy::unnecessary_literal_bound)]
pub trait Strategy: Send + Sync {
    /// Select a model for the given query profile
    fn route(
        &self,
        profile: &QueryProfile,
        registry: &ModelRegistry,
        feedback: Option<&FeedbackTracker>,
    ) -> Result<RoutingDecision, RoutingError>;

    /// Human-readable strategy name
    fn name(&self) -> &'static str;
}

/// Threshold strategy wrapper
pub struct ThresholdStrategy {
    config: synapse_config::ThresholdConfig,
}

#[allow(clippy::unnecessary_literal_bound)]
impl Strategy for ThresholdStrategy {
    fn route(
        &self,
        profile: &QueryProfile,
        registry: &ModelRegistry,
        feedback: Option<&FeedbackTracker>,
    ) -> Result<RoutingDecision, RoutingError> {
        threshold::route(profile, registry, &self.config, feedback)
    }

    fn name(&self) -> &'static str {
        "threshold"
    }
}

/// Cost strategy wrapper
pub struct CostStrategy {
    config: synapse_config::CostConfig,
}

#[allow(clippy::unnecessary_literal_bound)]
impl Strategy for CostStrategy {
    fn route(
        &self,
        profile: &QueryProfile,
        registry: &ModelRegistry,
        feedback: Option<&FeedbackTracker>,
    ) -> Result<RoutingDecision, RoutingError> {
        cost::route(profile, registry, &self.config, feedback)
    }

    fn name(&self) -> &'static str {
        "cost"
    }
}

/// Cascade strategy wrapper
pub struct CascadeStrategy {
    config: synapse_config::CascadeConfig,
}

#[allow(clippy::unnecessary_literal_bound)]
impl Strategy for CascadeStrategy {
    fn route(
        &self,
        profile: &QueryProfile,
        registry: &ModelRegistry,
        feedback: Option<&FeedbackTracker>,
    ) -> Result<RoutingDecision, RoutingError> {
        cascade::route(profile, registry, &self.config, feedback)
    }

    fn name(&self) -> &'static str {
        "cascade"
    }
}

/// Score strategy wrapper
pub struct ScoreStrategy {
    config: synapse_config::ScoreConfig,
}

#[allow(clippy::unnecessary_literal_bound)]
impl Strategy for ScoreStrategy {
    fn route(
        &self,
        profile: &QueryProfile,
        registry: &ModelRegistry,
        feedback: Option<&FeedbackTracker>,
    ) -> Result<RoutingDecision, RoutingError> {
        score::route(profile, registry, &self.config, feedback)
    }

    fn name(&self) -> &'static str {
        "score"
    }
}

/// Registry of available routing strategies
pub struct StrategyRegistry {
    strategies: HashMap<String, Box<dyn Strategy>>,
}

impl StrategyRegistry {
    /// Build from config with built-in strategies
    pub fn from_config(config: &RoutingConfig) -> Self {
        let mut strategies: HashMap<String, Box<dyn Strategy>> = HashMap::new();

        strategies.insert(
            "threshold".to_owned(),
            Box::new(ThresholdStrategy {
                config: config.threshold.clone(),
            }),
        );
        strategies.insert(
            "cost".to_owned(),
            Box::new(CostStrategy {
                config: config.cost.clone(),
            }),
        );
        strategies.insert(
            "cascade".to_owned(),
            Box::new(CascadeStrategy {
                config: config.cascade.clone(),
            }),
        );
        strategies.insert(
            "score".to_owned(),
            Box::new(ScoreStrategy {
                config: config.score.clone(),
            }),
        );

        Self { strategies }
    }

    /// Register a custom strategy
    pub fn register(&mut self, name: &str, strategy: Box<dyn Strategy>) {
        self.strategies.insert(name.to_owned(), strategy);
    }

    /// Get a strategy by name
    pub fn get(&self, name: &str) -> Option<&dyn Strategy> {
        self.strategies.get(name).map(AsRef::as_ref)
    }

    /// Resolve the strategy name from config
    pub const fn resolve_name(config: &RoutingConfig) -> &str {
        match &config.strategy {
            synapse_config::RoutingStrategy::Threshold => "threshold",
            synapse_config::RoutingStrategy::Cost => "cost",
            synapse_config::RoutingStrategy::Cascade => "cascade",
            synapse_config::RoutingStrategy::Score => "score",
            synapse_config::RoutingStrategy::Custom(name) => name.as_str(),
        }
    }
}

impl std::fmt::Debug for StrategyRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StrategyRegistry")
            .field("strategies", &self.strategies.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Complexity, RequiredCapabilities, TaskType};
    use synapse_config::{ModelCapabilities, ModelProfileConfig};

    fn test_registry() -> ModelRegistry {
        ModelRegistry::from_config(&[
            ModelProfileConfig {
                provider: "anthropic".to_owned(),
                model: "claude-sonnet".to_owned(),
                context_window: 200_000,
                input_per_mtok: 3.0,
                output_per_mtok: 15.0,
                quality: 0.92,
                capabilities: ModelCapabilities::default(),
            },
            ModelProfileConfig {
                provider: "openai".to_owned(),
                model: "gpt-4o-mini".to_owned(),
                context_window: 128_000,
                input_per_mtok: 0.15,
                output_per_mtok: 0.60,
                quality: 0.78,
                capabilities: ModelCapabilities::default(),
            },
        ])
    }

    fn test_profile() -> QueryProfile {
        QueryProfile {
            estimated_input_tokens: 50,
            task_type: TaskType::SimpleQa,
            complexity: Complexity::Low,
            requires_tool_use: false,
            required_capabilities: RequiredCapabilities::default(),
            message_count: 1,
            has_system_prompt: false,
        }
    }

    #[test]
    fn builtin_strategies_dispatch_via_trait() {
        let config = RoutingConfig::default();
        let strategy_reg = StrategyRegistry::from_config(&config);
        let model_reg = test_registry();

        let strategy = strategy_reg.get("threshold").unwrap();
        let decision = strategy.route(&test_profile(), &model_reg, None).unwrap();
        assert_eq!(decision.model, "gpt-4o-mini");
    }

    #[test]
    fn custom_strategy_registration() {
        // Register a custom strategy that always picks the best quality model
        struct AlwaysBest;
        impl Strategy for AlwaysBest {
            fn route(
                &self,
                _profile: &QueryProfile,
                registry: &ModelRegistry,
                _feedback: Option<&FeedbackTracker>,
            ) -> Result<RoutingDecision, RoutingError> {
                let best = registry.best_quality().ok_or(RoutingError::NoProfiles)?;
                Ok(RoutingDecision {
                    provider: best.provider.clone(),
                    model: best.model.clone(),
                    reason: crate::RoutingReason::BestQuality,
                    alternatives: vec![],
                })
            }
            fn name(&self) -> &'static str {
                "always-best"
            }
        }

        let config = RoutingConfig::default();
        let mut strategy_reg = StrategyRegistry::from_config(&config);
        strategy_reg.register("always-best", Box::new(AlwaysBest));

        let model_reg = test_registry();
        let strategy = strategy_reg.get("always-best").unwrap();
        let decision = strategy.route(&test_profile(), &model_reg, None).unwrap();
        assert_eq!(decision.model, "claude-sonnet");
    }

    #[test]
    fn unknown_strategy_returns_none() {
        let config = RoutingConfig::default();
        let strategy_reg = StrategyRegistry::from_config(&config);
        assert!(strategy_reg.get("nonexistent").is_none());
    }

    #[test]
    fn resolve_name_builtins() {
        let mut config = RoutingConfig::default();
        assert_eq!(StrategyRegistry::resolve_name(&config), "threshold");

        config.strategy = synapse_config::RoutingStrategy::Cost;
        assert_eq!(StrategyRegistry::resolve_name(&config), "cost");

        config.strategy = synapse_config::RoutingStrategy::Custom("my-strategy".to_owned());
        assert_eq!(StrategyRegistry::resolve_name(&config), "my-strategy");
    }
}
