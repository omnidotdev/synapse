//! ONNX-based ML routing strategy
//!
//! Load a pre-trained ONNX model that predicts the *required quality* for a
//! query (a single scalar in 0.0 to 1.0), then pick the cheapest capability
//! satisfying model meeting that bar. Predicting a target rather than a model
//! class keeps the model decoupled from the registry, so adding or removing
//! models never silently misaligns the output. Training happens offline in
//! Python (see `ml/`); inference runs in Rust via the `ort` crate
//!
//! Enable with the `onnx` feature flag

use crate::RoutingDecision;
use crate::analysis::QueryProfile;
use crate::error::RoutingError;
use crate::feedback::FeedbackTracker;
use crate::registry::ModelRegistry;
use crate::strategy::Strategy;

#[cfg(feature = "onnx")]
use ort::session::Session;
#[cfg(feature = "onnx")]
use std::sync::Mutex;

/// Number of input features extracted from a `QueryProfile`
#[cfg(feature = "onnx")]
const NUM_FEATURES: usize = 8;

/// ONNX ML-based routing strategy
///
/// When the `onnx` feature is enabled, holds a loaded `ort::Session`
/// for inference. Otherwise acts as a placeholder that returns a
/// feature-not-available error
#[derive(Debug)]
pub struct OnnxStrategy {
    #[cfg(feature = "onnx")]
    session: Mutex<Session>,
    #[cfg(not(feature = "onnx"))]
    _phantom: (),
}

impl OnnxStrategy {
    /// Load an ONNX model from disk
    ///
    /// # Errors
    ///
    /// Returns `RoutingError::FeatureNotAvailable` when the `onnx` feature
    /// is not enabled. Returns `RoutingError::AnalysisFailed` if the model
    /// file cannot be loaded
    #[cfg(feature = "onnx")]
    pub fn load(model_path: &str) -> Result<Self, RoutingError> {
        let session = Session::builder()
            .and_then(|mut builder| builder.commit_from_file(model_path))
            .map_err(|e| RoutingError::AnalysisFailed(format!("failed to load ONNX model: {e}")))?;

        tracing::info!(path = %model_path, "loaded ONNX routing model");

        Ok(Self {
            session: Mutex::new(session),
        })
    }

    /// Load an ONNX model from disk
    ///
    /// # Errors
    ///
    /// Returns `RoutingError::FeatureNotAvailable` when the `onnx` feature
    /// is not enabled
    #[cfg(not(feature = "onnx"))]
    pub fn load(_model_path: &str) -> Result<Self, RoutingError> {
        Err(RoutingError::FeatureNotAvailable {
            feature: "onnx".to_owned(),
        })
    }
}

impl Strategy for OnnxStrategy {
    // The session guard must live across run + tensor extraction (the output
    // tensor borrows the session), so it cannot be dropped any earlier than the
    // inference scope already does
    #[cfg_attr(feature = "onnx", allow(clippy::significant_drop_tightening))]
    fn route(
        &self,
        profile: &QueryProfile,
        registry: &ModelRegistry,
        _feedback: Option<&FeedbackTracker>,
    ) -> Result<RoutingDecision, RoutingError> {
        #[cfg(feature = "onnx")]
        {
            if registry.profiles().is_empty() {
                return Err(RoutingError::NoProfiles);
            }

            // Extract features and run inference to predict the required quality
            let features = profile_to_features(profile);
            let input_array = ndarray::Array2::from_shape_vec((1, NUM_FEATURES), features)
                .map_err(|e| RoutingError::AnalysisFailed(format!("failed to build input tensor: {e}")))?;

            // Run inference inside a scope that yields an owned scalar, so the
            // session mutex guard is released the instant inference is done and
            // is never held across model selection
            let raw_quality: f32 = {
                let input_ref = ort::value::TensorRef::from_array_view(&input_array)
                    .map_err(|e| RoutingError::AnalysisFailed(format!("failed to prepare ONNX inputs: {e}")))?;

                let mut session = self
                    .session
                    .lock()
                    .map_err(|e| RoutingError::AnalysisFailed(format!("failed to lock ONNX session: {e}")))?;

                let outputs = session
                    .run(ort::inputs![input_ref])
                    .map_err(|e| RoutingError::AnalysisFailed(format!("ONNX inference failed: {e}")))?;

                if outputs.len() == 0 {
                    return Err(RoutingError::AnalysisFailed(
                        "ONNX model returned no outputs".to_owned(),
                    ));
                }

                let (_shape, data) = outputs[0]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| RoutingError::AnalysisFailed(format!("failed to extract output tensor: {e}")))?;

                *data.first().ok_or_else(|| {
                    RoutingError::AnalysisFailed("ONNX model returned an empty output tensor".to_owned())
                })?
            };

            // The model emits a single scalar: the predicted required quality.
            // Clamp into the valid range to stay robust to a mis-scaled model
            let required_quality = f64::from(raw_quality).clamp(0.0, 1.0);

            // `registry` is already capability-filtered by the routing engine, so
            // the cheapest model clearing the predicted bar is a valid choice.
            // Fall back to best available quality when nothing clears it
            let (selected, reason) = if let Some(model) = registry.cheapest_above_quality(required_quality) {
                (model, crate::RoutingReason::OnnxRouted)
            } else {
                tracing::warn!(
                    required_quality,
                    "no model meets ONNX-predicted quality, falling back to best available"
                );
                (
                    registry.best_quality().ok_or(RoutingError::NoProfiles)?,
                    crate::RoutingReason::BestQuality,
                )
            };

            let alternatives = registry
                .profiles()
                .iter()
                .filter(|p| p.provider != selected.provider || p.model != selected.model)
                .map(|p| (p.provider.clone(), p.model.clone()))
                .collect();

            tracing::debug!(
                selected_model = %selected.id(),
                required_quality,
                "ONNX strategy routed by predicted quality"
            );

            Ok(RoutingDecision {
                provider: selected.provider.clone(),
                model: selected.model.clone(),
                reason,
                alternatives,
            })
        }

        #[cfg(not(feature = "onnx"))]
        {
            let _ = (profile, registry);
            Err(RoutingError::FeatureNotAvailable {
                feature: "onnx".to_owned(),
            })
        }
    }

    fn name(&self) -> &'static str {
        "onnx"
    }
}

#[cfg(feature = "onnx")]
/// Convert a `QueryProfile` into a fixed-size feature vector for ONNX inference
///
/// Produces `NUM_FEATURES` (8) f32 values:
/// 0. `estimated_input_tokens / 100_000.0` (normalized token count)
/// 1. `task_type` ordinal (0 to 5)
/// 2. `complexity` ordinal (0 to 2)
/// 3. `requires_tool_use` (0.0 or 1.0)
/// 4. `vision` capability (0.0 or 1.0)
/// 5. `long_context` capability (0.0 or 1.0)
/// 6. `message_count / 50.0` (normalized message count)
/// 7. `has_system_prompt` (0.0 or 1.0)
#[cfg(feature = "onnx")]
fn profile_to_features(profile: &QueryProfile) -> Vec<f32> {
    use crate::analysis::{Complexity, TaskType};

    let task_ordinal = match profile.task_type {
        TaskType::SimpleQa => 0.0,
        TaskType::General => 1.0,
        TaskType::Creative => 2.0,
        TaskType::Analysis => 3.0,
        TaskType::Code => 4.0,
        TaskType::Math => 5.0,
    };

    let complexity_ordinal = match profile.complexity {
        Complexity::Low => 0.0,
        Complexity::Medium => 1.0,
        Complexity::High => 2.0,
    };

    vec![
        profile.estimated_input_tokens as f32 / 100_000.0,
        task_ordinal,
        complexity_ordinal,
        if profile.requires_tool_use { 1.0 } else { 0.0 },
        if profile.required_capabilities.vision { 1.0 } else { 0.0 },
        if profile.required_capabilities.long_context {
            1.0
        } else {
            0.0
        },
        profile.message_count as f32 / 50.0,
        if profile.has_system_prompt { 1.0 } else { 0.0 },
    ]
}

#[cfg(all(test, not(feature = "onnx")))]
mod tests {
    use super::*;

    #[test]
    fn load_without_feature_is_unavailable() {
        // With the `onnx` feature compiled out, loading must fail gracefully
        // rather than panic, so the registry can skip registering the strategy
        let err = OnnxStrategy::load("does-not-matter.onnx").unwrap_err();
        assert!(matches!(err, RoutingError::FeatureNotAvailable { .. }));
    }
}
