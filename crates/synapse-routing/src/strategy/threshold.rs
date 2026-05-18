//! Threshold-based routing strategy
//!
//! Adapted from `LLMRouter`'s `HybridLLMRouter`. Routes queries to a cheap
//! or frontier model based on heuristic complexity classification.
//! Low complexity → cheapest model above quality floor.
//! High complexity → best quality model.

use synapse_config::ThresholdConfig;

use crate::analysis::{Complexity, QueryProfile, RequiredCapabilities};
use crate::error::RoutingError;
use crate::feedback::FeedbackTracker;
use crate::registry::{ModelProfile, ModelRegistry};
use crate::scoring::effective_quality;
use crate::{RoutingDecision, RoutingReason};

/// Check if a model satisfies the required capabilities
const fn model_satisfies_caps(profile: &ModelProfile, required: &RequiredCapabilities) -> bool {
    (!required.tool_calling || profile.tool_calling)
        && (!required.vision || profile.vision)
        && (!required.long_context || profile.long_context)
}

/// Route a query using the threshold strategy
pub fn route(
    profile: &QueryProfile,
    registry: &ModelRegistry,
    config: &ThresholdConfig,
    feedback: Option<&FeedbackTracker>,
) -> Result<RoutingDecision, RoutingError> {
    // If explicit models are configured, try to use them directly
    if let (Some(low), Some(high)) = (&config.low_complexity_model, &config.high_complexity_model) {
        let (selected, reason) = match profile.complexity {
            Complexity::Low => (low.clone(), RoutingReason::LowComplexity),
            Complexity::Medium | Complexity::High => (high.clone(), RoutingReason::HighComplexity),
        };

        if let Some((provider, model)) = selected.split_once('/') {
            // Validate configured model satisfies required capabilities
            let satisfies = registry
                .find(provider, model)
                .is_none_or(|p| model_satisfies_caps(p, &profile.required_capabilities));

            if satisfies {
                let alt = if selected == *low { high } else { low };
                let alternatives = alt
                    .split_once('/')
                    .map(|(p, m)| vec![(p.to_owned(), m.to_owned())])
                    .unwrap_or_default();

                return Ok(RoutingDecision {
                    provider: provider.to_owned(),
                    model: model.to_owned(),
                    reason,
                    alternatives,
                });
            }

            tracing::warn!(
                configured_model = %selected,
                capabilities = ?profile.required_capabilities,
                "configured threshold model lacks required capabilities, falling back to registry"
            );
        }
    }

    // Fall back to registry-based selection, using feedback-adjusted quality
    let selected = match profile.complexity {
        Complexity::Low => {
            // Find cheapest model whose feedback-adjusted quality meets the floor
            registry
                .by_cost()
                .into_iter()
                .find(|p| effective_quality(p, feedback) >= config.quality_floor)
                .ok_or(RoutingError::NoProfiles)?
        }
        Complexity::Medium | Complexity::High => registry.best_quality().ok_or(RoutingError::NoProfiles)?,
    };

    let reason = match profile.complexity {
        Complexity::Low => RoutingReason::LowComplexity,
        Complexity::Medium | Complexity::High => RoutingReason::HighComplexity,
    };

    // Build alternatives from other models in the registry
    let alternatives = registry
        .profiles()
        .iter()
        .filter(|p| p.provider != selected.provider || p.model != selected.model)
        .map(|p| (p.provider.clone(), p.model.clone()))
        .collect();

    Ok(RoutingDecision {
        provider: selected.provider.clone(),
        model: selected.model.clone(),
        reason,
        alternatives,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{RequiredCapabilities, TaskType};
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

    #[test]
    fn low_complexity_picks_cheap() {
        let registry = test_registry();
        let profile = QueryProfile {
            estimated_input_tokens: 50,
            task_type: TaskType::SimpleQa,
            complexity: Complexity::Low,
            requires_tool_use: false,
            required_capabilities: RequiredCapabilities::default(),
            message_count: 1,
            has_system_prompt: false,
        };
        let config = ThresholdConfig::default();
        let decision = route(&profile, &registry, &config, None).unwrap();
        assert_eq!(decision.model, "gpt-4o-mini");
        assert_eq!(decision.reason, RoutingReason::LowComplexity);
    }

    #[test]
    fn high_complexity_picks_best() {
        let registry = test_registry();
        let profile = QueryProfile {
            estimated_input_tokens: 5000,
            task_type: TaskType::Code,
            complexity: Complexity::High,
            requires_tool_use: false,
            required_capabilities: RequiredCapabilities::default(),
            message_count: 1,
            has_system_prompt: false,
        };
        let config = ThresholdConfig::default();
        let decision = route(&profile, &registry, &config, None).unwrap();
        assert_eq!(decision.model, "claude-sonnet");
        assert_eq!(decision.reason, RoutingReason::HighComplexity);
    }
}
