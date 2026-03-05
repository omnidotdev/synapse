//! Multi-objective score routing strategy
//!
//! Combines quality, cost, and latency into a single weighted score.
//! Optionally penalizes models with high error rates observed at runtime.

use synapse_config::ScoreConfig;

use crate::analysis::QueryProfile;
use crate::error::RoutingError;
use crate::feedback::FeedbackTracker;
use crate::registry::ModelRegistry;
use crate::{RoutingDecision, RoutingReason};

/// Default latency assumption when no data is available (ms)
const DEFAULT_LATENCY_MS: f64 = 2000.0;

/// Route a query using multi-objective score optimization
pub fn route(
    _profile: &QueryProfile,
    registry: &ModelRegistry,
    config: &ScoreConfig,
    feedback: Option<&FeedbackTracker>,
) -> Result<RoutingDecision, RoutingError> {
    let profiles = registry.profiles();
    if profiles.is_empty() {
        return Err(RoutingError::NoProfiles);
    }

    // Compute normalization bounds
    let max_cost = profiles
        .iter()
        .map(|p| p.input_per_mtok + p.output_per_mtok)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_latency = profiles
        .iter()
        .map(|p| resolve_latency(p, feedback))
        .fold(f64::NEG_INFINITY, f64::max);

    // Guard against zero denominators
    let max_cost = if max_cost <= 0.0 { 1.0 } else { max_cost };
    let max_latency = if max_latency <= 0.0 { 1.0 } else { max_latency };

    // Score each model
    let mut scored: Vec<_> = profiles
        .iter()
        .map(|p| {
            let cost = p.input_per_mtok + p.output_per_mtok;
            let latency = resolve_latency(p, feedback);

            let cost_score = 1.0 - cost / max_cost;
            let latency_score = 1.0 - latency / max_latency;
            let raw = config.weight_quality.mul_add(
                p.quality,
                config
                    .weight_cost
                    .mul_add(cost_score, config.weight_latency * latency_score),
            );

            // Apply error penalty from feedback
            let error_rate = feedback.map_or(0.0, |f| {
                let snap = f.snapshot(&p.provider, &p.model);
                if snap.sample_count >= config.min_samples {
                    snap.error_rate.unwrap_or(0.0)
                } else {
                    0.0
                }
            });

            let final_score = config.error_penalty.mul_add(-error_rate, 1.0) * raw;
            (p, final_score)
        })
        .collect();

    // Sort by score descending
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let (selected, _score) = scored.first().ok_or(RoutingError::NoProfiles)?;

    let alternatives = scored
        .iter()
        .skip(1)
        .map(|(p, _)| (p.provider.clone(), p.model.clone()))
        .collect();

    Ok(RoutingDecision {
        provider: selected.provider.clone(),
        model: selected.model.clone(),
        reason: RoutingReason::ScoreOptimized,
        alternatives,
    })
}

/// Resolve latency for a model from feedback, profile, or default
fn resolve_latency(profile: &crate::registry::ModelProfile, feedback: Option<&FeedbackTracker>) -> f64 {
    // Prefer live feedback data
    if let Some(tracker) = feedback {
        let snap = tracker.snapshot(&profile.provider, &profile.model);
        if let Some(p50) = snap.latency_p50_ms {
            return p50;
        }
    }

    // Fall back to profile's observed latency
    profile.observed_latency_p50_ms.unwrap_or(DEFAULT_LATENCY_MS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Complexity, RequiredCapabilities, TaskType};
    use synapse_config::{ModelCapabilities, ModelProfileConfig};

    fn test_registry() -> ModelRegistry {
        ModelRegistry::from_config(&[
            ModelProfileConfig {
                provider: "fast".to_owned(),
                model: "small".to_owned(),
                context_window: 32_000,
                input_per_mtok: 0.1,
                output_per_mtok: 0.3,
                quality: 0.70,
                capabilities: ModelCapabilities::default(),
            },
            ModelProfileConfig {
                provider: "frontier".to_owned(),
                model: "big".to_owned(),
                context_window: 200_000,
                input_per_mtok: 10.0,
                output_per_mtok: 30.0,
                quality: 0.95,
                capabilities: ModelCapabilities::default(),
            },
        ])
    }

    fn test_profile() -> QueryProfile {
        QueryProfile {
            estimated_input_tokens: 500,
            task_type: TaskType::General,
            complexity: Complexity::Medium,
            requires_tool_use: false,
            required_capabilities: RequiredCapabilities::default(),
            message_count: 1,
            has_system_prompt: false,
        }
    }

    #[test]
    fn balanced_weights_prefer_value() {
        let registry = test_registry();
        let config = ScoreConfig::default();
        let decision = route(&test_profile(), &registry, &config, None).unwrap();
        // With balanced weights, the cheap model's cost advantage should
        // help it compete. The exact winner depends on weights
        assert_eq!(decision.reason, RoutingReason::ScoreOptimized);
        assert!(!decision.alternatives.is_empty());
    }

    #[test]
    fn quality_heavy_picks_best() {
        let registry = test_registry();
        let config = ScoreConfig {
            weight_quality: 1.0,
            weight_cost: 0.0,
            weight_latency: 0.0,
            ..ScoreConfig::default()
        };
        let decision = route(&test_profile(), &registry, &config, None).unwrap();
        assert_eq!(decision.model, "big");
    }

    #[test]
    fn cost_heavy_picks_cheap() {
        let registry = test_registry();
        let config = ScoreConfig {
            weight_quality: 0.0,
            weight_cost: 1.0,
            weight_latency: 0.0,
            ..ScoreConfig::default()
        };
        let decision = route(&test_profile(), &registry, &config, None).unwrap();
        assert_eq!(decision.model, "small");
    }

    #[test]
    fn error_penalty_demotes() {
        use crate::feedback::RequestFeedback;
        use std::time::Duration;

        let registry = test_registry();
        let tracker = FeedbackTracker::new();

        // Record high error rate for the big model
        for i in 0..20 {
            tracker.record(&RequestFeedback {
                provider: "frontier".to_owned(),
                model: "big".to_owned(),
                latency: Duration::from_millis(100),
                success: i < 2, // 90% error rate
                input_tokens: None,
                output_tokens: None,
            });
        }

        // Quality-heavy but error penalty should demote it
        let config = ScoreConfig {
            weight_quality: 0.8,
            weight_cost: 0.1,
            weight_latency: 0.1,
            error_penalty: 1.0,
            min_samples: 10,
        };
        let decision = route(&test_profile(), &registry, &config, Some(&tracker)).unwrap();
        assert_eq!(decision.model, "small");
    }
}
