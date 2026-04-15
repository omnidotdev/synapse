//! Provider failover with health tracking and automatic recovery
//!
//! Wraps routing decisions: when a provider's error rate exceeds a
//! threshold, skip it and promote the next alternative

use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::RoutingDecision;
use crate::error::RoutingError;
use crate::feedback::FeedbackTracker;

/// Track which providers are currently marked unhealthy
#[derive(Debug)]
pub struct FailoverState {
    /// Provider name -> when it was marked down
    down_providers: DashMap<String, Instant>,
    /// How long to avoid a downed provider before retrying
    recovery_window: Duration,
    /// Error rate threshold (0.0 to 1.0) above which a provider is considered down
    error_threshold: f64,
}

impl FailoverState {
    /// Create a new failover state tracker
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use synapse_routing::FailoverState;
    ///
    /// let state = FailoverState::new(Duration::from_secs(60), 0.5);
    /// ```
    pub fn new(recovery_window: Duration, error_threshold: f64) -> Self {
        Self {
            down_providers: DashMap::new(),
            recovery_window,
            error_threshold,
        }
    }

    /// Check feedback error rates and update provider health state
    ///
    /// Marks providers as down when their error rate exceeds the threshold,
    /// and prunes providers whose recovery window has elapsed
    pub fn update_health(&self, feedback: &FeedbackTracker, providers: &[String]) {
        // Prune recovered providers
        self.down_providers
            .retain(|_, marked_at| marked_at.elapsed() < self.recovery_window);

        // Check each provider's error rate
        for provider in providers {
            if let Some(error_rate) = feedback.error_rate(provider, "")
                && error_rate >= self.error_threshold
            {
                tracing::warn!(
                    provider = %provider,
                    error_rate = %error_rate,
                    threshold = %self.error_threshold,
                    "marking provider as down"
                );
                self.down_providers.insert(provider.clone(), Instant::now());
            }

            // Also check provider-level error rate by looking at "provider/*" pattern
            // The FeedbackTracker uses "provider/model" keys, so we check with
            // an empty model to get the provider-level rate
        }
    }

    /// Check if a provider is healthy
    ///
    /// Returns `true` if the provider is not in the down list, or if its
    /// recovery window has elapsed
    pub fn is_healthy(&self, provider: &str) -> bool {
        self.down_providers
            .get(provider)
            .is_none_or(|marked_at| marked_at.elapsed() >= self.recovery_window)
    }

    /// Apply failover logic to a routing decision
    ///
    /// If the primary provider is unhealthy, promotes the first healthy
    /// alternative. Returns an error if all providers are down.
    ///
    /// # Errors
    ///
    /// Returns `RoutingError::AllProvidersDown` if the primary and all
    /// alternatives are currently unhealthy
    pub fn apply(&self, decision: RoutingDecision) -> Result<RoutingDecision, RoutingError> {
        // Primary is healthy, pass through
        if self.is_healthy(&decision.provider) {
            return Ok(decision);
        }

        tracing::info!(
            primary = %decision.provider,
            "primary provider unhealthy, checking alternatives"
        );

        // Try each alternative in order
        for (alt_provider, alt_model) in &decision.alternatives {
            if self.is_healthy(alt_provider) {
                tracing::info!(
                    failover_to = %alt_provider,
                    model = %alt_model,
                    "failing over to healthy alternative"
                );

                // Build new alternatives: original primary + remaining alternatives
                let mut new_alternatives: Vec<(String, String)> =
                    vec![(decision.provider.clone(), decision.model.clone())];
                for (p, m) in &decision.alternatives {
                    if p != alt_provider || m != alt_model {
                        new_alternatives.push((p.clone(), m.clone()));
                    }
                }

                return Ok(RoutingDecision {
                    provider: alt_provider.clone(),
                    model: alt_model.clone(),
                    reason: decision.reason,
                    alternatives: new_alternatives,
                });
            }
        }

        Err(RoutingError::AllProvidersDown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RoutingReason;

    fn test_decision() -> RoutingDecision {
        RoutingDecision {
            provider: "primary".to_owned(),
            model: "model-a".to_owned(),
            reason: RoutingReason::BestQuality,
            alternatives: vec![
                ("secondary".to_owned(), "model-b".to_owned()),
                ("tertiary".to_owned(), "model-c".to_owned()),
            ],
        }
    }

    #[test]
    fn healthy_provider_passes_through() {
        let state = FailoverState::new(Duration::from_secs(60), 0.5);
        let decision = test_decision();
        let result = state.apply(decision).unwrap();

        assert_eq!(result.provider, "primary");
        assert_eq!(result.model, "model-a");
    }

    #[test]
    fn downed_provider_triggers_alternative() {
        let state = FailoverState::new(Duration::from_secs(60), 0.5);

        // Mark primary as down
        state.down_providers.insert("primary".to_owned(), Instant::now());

        let decision = test_decision();
        let result = state.apply(decision).unwrap();

        assert_eq!(result.provider, "secondary");
        assert_eq!(result.model, "model-b");
        // Original primary should be in alternatives
        assert!(result.alternatives.iter().any(|(p, _)| p == "primary"));
    }

    #[test]
    fn recovery_window_restores_provider() {
        // Use a very short recovery window
        let state = FailoverState::new(Duration::from_millis(1), 0.5);

        // Mark primary as down in the past
        state.down_providers.insert(
            "primary".to_owned(),
            Instant::now().checked_sub(Duration::from_millis(10)).unwrap(),
        );

        // Provider should be healthy again
        assert!(state.is_healthy("primary"));

        let decision = test_decision();
        let result = state.apply(decision).unwrap();
        assert_eq!(result.provider, "primary");
    }

    #[test]
    fn all_providers_down_returns_error() {
        let state = FailoverState::new(Duration::from_secs(60), 0.5);

        // Mark all providers as down
        state.down_providers.insert("primary".to_owned(), Instant::now());
        state.down_providers.insert("secondary".to_owned(), Instant::now());
        state.down_providers.insert("tertiary".to_owned(), Instant::now());

        let decision = test_decision();
        let result = state.apply(decision);

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RoutingError::AllProvidersDown));
    }

    #[test]
    fn update_health_marks_providers_down() {
        let state = FailoverState::new(Duration::from_secs(60), 0.5);
        let feedback = FeedbackTracker::new();

        // Record failures to push error rate above threshold
        for _ in 0..10 {
            feedback.record(&crate::feedback::RequestFeedback {
                provider: "failing-provider".to_owned(),
                model: String::new(),
                latency: Duration::from_millis(100),
                success: false,
                input_tokens: None,
                output_tokens: None,
            });
        }

        let providers = vec!["failing-provider".to_owned()];
        state.update_health(&feedback, &providers);

        assert!(!state.is_healthy("failing-provider"));
    }

    #[test]
    fn update_health_prunes_recovered() {
        let state = FailoverState::new(Duration::from_millis(1), 0.5);

        // Insert a provider that was marked down long ago
        state.down_providers.insert(
            "recovered".to_owned(),
            Instant::now().checked_sub(Duration::from_millis(10)).unwrap(),
        );

        let feedback = FeedbackTracker::new();
        state.update_health(&feedback, &[]);

        // Should have been pruned
        assert!(state.is_healthy("recovered"));
        assert!(state.down_providers.is_empty());
    }
}
