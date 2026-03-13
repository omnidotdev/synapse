//! Cascade routing strategy
//!
//! Adapted from `LLMRouter`'s `AutomixRouter`. Tries a cheap model first,
//! then evaluates response confidence heuristically. If confidence is
//! below threshold, escalates to a stronger model.
//!
//! Unlike `AutoMix`'s LLM-based self-verification, this uses simple
//! heuristic confidence signals (response length, hedging language).

use synapse_config::CascadeConfig;

use crate::analysis::QueryProfile;
use crate::error::RoutingError;
use crate::feedback::FeedbackTracker;
use crate::registry::ModelRegistry;
use crate::{RoutingDecision, RoutingReason};

/// Route a query using the cascade strategy
///
/// Returns the initial (cheap) model. The caller is responsible for
/// calling `evaluate_for_escalation` after receiving the response
/// and potentially re-routing to the escalation model.
pub fn route(
    _profile: &QueryProfile,
    registry: &ModelRegistry,
    config: &CascadeConfig,
    _feedback: Option<&FeedbackTracker>,
) -> Result<RoutingDecision, RoutingError> {
    let (provider, model) = resolve_initial(registry, config)?;
    let (esc_provider, esc_model) = resolve_escalation(registry, config)?;

    Ok(RoutingDecision {
        provider,
        model,
        reason: RoutingReason::CascadeInitial,
        alternatives: vec![(esc_provider, esc_model)],
    })
}

/// Evaluate a buffered streaming response for confidence
///
/// Wrapper around `should_escalate` for use in the streaming cascade path.
/// Returns `true` if the buffered response is confident enough to keep.
pub fn evaluate_buffered_response(text: &str, query_tokens: usize, confidence_threshold: f64) -> bool {
    !should_escalate(text, query_tokens, confidence_threshold)
}

/// Evaluate whether a response should be escalated to a stronger model
///
/// Returns `true` if confidence is below threshold and escalation is warranted.
pub fn should_escalate(response_text: &str, query_tokens: usize, confidence_threshold: f64) -> bool {
    let confidence = estimate_confidence(response_text, query_tokens);
    confidence < confidence_threshold
}

/// Heuristic confidence estimation
///
/// Returns a score between 0.0 and 1.0 based on:
/// - Response length relative to query complexity
/// - Presence of hedging language
/// - Structural completeness
fn estimate_confidence(response_text: &str, query_tokens: usize) -> f64 {
    let mut score: f64 = 0.7; // Base confidence

    // Penalize very short responses to complex queries
    let response_words = response_text.split_whitespace().count();
    if query_tokens > 500 && response_words < 20 {
        score -= 0.3;
    }

    // Penalize hedging language
    let hedging = [
        "i'm not sure",
        "i don't know",
        "i'm uncertain",
        "it's unclear",
        "i cannot",
        "i can't determine",
        "i may be wrong",
        "this might not be",
    ];
    let lower = response_text.to_lowercase();
    for phrase in &hedging {
        if lower.contains(phrase) {
            score -= 0.15;
        }
    }

    // Penalize empty or near-empty responses
    if response_text.trim().is_empty() {
        score = 0.0;
    }

    score.clamp(0.0, 1.0)
}

fn resolve_initial(registry: &ModelRegistry, config: &CascadeConfig) -> Result<(String, String), RoutingError> {
    if let Some(ref configured) = config.initial_model {
        return split_model(configured, "cascade initial");
    }

    // Default to cheapest model
    let cheapest = registry.by_cost().into_iter().next().ok_or(RoutingError::NoProfiles)?;
    Ok((cheapest.provider.clone(), cheapest.model.clone()))
}

fn resolve_escalation(registry: &ModelRegistry, config: &CascadeConfig) -> Result<(String, String), RoutingError> {
    if let Some(ref configured) = config.escalation_model {
        return split_model(configured, "cascade escalation");
    }

    // Default to best quality model
    let best = registry.best_quality().ok_or(RoutingError::NoProfiles)?;
    Ok((best.provider.clone(), best.model.clone()))
}

fn split_model(model: &str, context: &str) -> Result<(String, String), RoutingError> {
    let (provider, model_id) = model.split_once('/').ok_or_else(|| RoutingError::NoModelAvailable {
        class: context.to_owned(),
    })?;
    Ok((provider.to_owned(), model_id.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confident_response_no_escalation() {
        assert!(!should_escalate(
            "The answer is 42. This is well-established.",
            100,
            0.5
        ));
    }

    #[test]
    fn hedging_response_escalates() {
        assert!(should_escalate(
            "I'm not sure about this, I'm uncertain of the details",
            100,
            0.5
        ));
    }

    #[test]
    fn empty_response_escalates() {
        assert!(should_escalate("", 100, 0.5));
    }

    #[test]
    fn short_response_to_complex_query_escalates() {
        assert!(should_escalate("Maybe.", 1000, 0.5));
    }
}
