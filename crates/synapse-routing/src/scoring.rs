//! Shared feedback-adjusted scoring
//!
//! Applies observed runtime performance data to model quality scores.
//! Conservative by design — feedback can only demote, never promote.

use crate::feedback::FeedbackTracker;
use crate::registry::ModelProfile;

/// Minimum observations before feedback adjusts scores
const MIN_FEEDBACK_SAMPLES: usize = 10;

/// Error rate above which quality is penalized
const ERROR_RATE_THRESHOLD: f64 = 0.10;

/// Multiplier applied to error rate when penalizing quality
const ERROR_PENALTY_FACTOR: f64 = 0.2;

/// Compute effective quality, optionally adjusted by runtime feedback
///
/// Without sufficient feedback data, returns the profile's base quality.
/// With feedback, penalizes models with high error rates. The penalty
/// is bounded so feedback can only demote, never promote.
pub fn effective_quality(profile: &ModelProfile, feedback: Option<&FeedbackTracker>) -> f64 {
    let base = profile.quality;

    let Some(tracker) = feedback else {
        return base;
    };

    let snap = tracker.snapshot(&profile.provider, &profile.model);

    if snap.sample_count < MIN_FEEDBACK_SAMPLES {
        return base;
    }

    let Some(error_rate) = snap.error_rate else {
        return base;
    };

    if error_rate <= ERROR_RATE_THRESHOLD {
        return base;
    }

    // Penalize proportionally to error rate
    error_rate.mul_add(-ERROR_PENALTY_FACTOR, base).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use synapse_config::{ModelCapabilities, ModelProfileConfig};

    use super::*;
    use crate::feedback::RequestFeedback;
    use crate::registry::ModelRegistry;

    fn test_profile() -> ModelProfile {
        let registry = ModelRegistry::from_config(&[ModelProfileConfig {
            provider: "test".to_owned(),
            model: "model-1".to_owned(),
            context_window: 128_000,
            input_per_mtok: 1.0,
            output_per_mtok: 2.0,
            quality: 0.90,
            capabilities: ModelCapabilities::default(),
        }]);
        registry.profiles()[0].clone()
    }

    #[test]
    fn no_feedback_returns_base() {
        let profile = test_profile();
        assert!((effective_quality(&profile, None) - 0.90).abs() < f64::EPSILON);
    }

    #[test]
    fn insufficient_samples_returns_base() {
        let profile = test_profile();
        let tracker = FeedbackTracker::new();

        // Record fewer than MIN_FEEDBACK_SAMPLES
        for _ in 0..5 {
            tracker.record(&RequestFeedback {
                provider: "test".to_owned(),
                model: "model-1".to_owned(),
                latency: Duration::from_millis(100),
                success: false,
                input_tokens: None,
                output_tokens: None,
            });
        }

        assert!((effective_quality(&profile, Some(&tracker)) - 0.90).abs() < f64::EPSILON);
    }

    #[test]
    fn high_error_rate_penalizes() {
        let profile = test_profile();
        let tracker = FeedbackTracker::new();

        // 5 successes, 15 failures = 75% error rate
        for i in 0..20 {
            tracker.record(&RequestFeedback {
                provider: "test".to_owned(),
                model: "model-1".to_owned(),
                latency: Duration::from_millis(100),
                success: i < 5,
                input_tokens: None,
                output_tokens: None,
            });
        }

        let q = effective_quality(&profile, Some(&tracker));
        // 0.90 - 0.75 * 0.2 = 0.90 - 0.15 = 0.75
        assert!((q - 0.75).abs() < 0.01);
    }

    #[test]
    fn low_error_rate_no_penalty() {
        let profile = test_profile();
        let tracker = FeedbackTracker::new();

        // 19 successes, 1 failure = 5% error rate (below threshold)
        for i in 0..20 {
            tracker.record(&RequestFeedback {
                provider: "test".to_owned(),
                model: "model-1".to_owned(),
                latency: Duration::from_millis(100),
                success: i < 19,
                input_tokens: None,
                output_tokens: None,
            });
        }

        assert!((effective_quality(&profile, Some(&tracker)) - 0.90).abs() < f64::EPSILON);
    }
}
