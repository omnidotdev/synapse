//! ONNX-based ML routing strategy
//!
//! Load a pre-trained ONNX model to classify query complexity and select
//! the optimal model. Training happens offline in Python; inference runs
//! in Rust via the `ort` crate
//!
//! Enable with the `onnx` feature flag

use crate::RoutingDecision;
use crate::analysis::QueryProfile;
use crate::error::RoutingError;
use crate::feedback::FeedbackTracker;
use crate::registry::ModelRegistry;
use crate::strategy::Strategy;

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
    session: ort::Session,
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
        let session = ort::Session::builder()
            .and_then(|builder| builder.commit_from_file(model_path))
            .map_err(|e| RoutingError::AnalysisFailed(format!("failed to load ONNX model: {e}")))?;

        tracing::info!(path = %model_path, "loaded ONNX routing model");

        Ok(Self { session })
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
    fn route(
        &self,
        _profile: &QueryProfile,
        registry: &ModelRegistry,
        _feedback: Option<&FeedbackTracker>,
    ) -> Result<RoutingDecision, RoutingError> {
        #[cfg(feature = "onnx")]
        {
            let profiles = registry.profiles();

            if profiles.is_empty() {
                return Err(RoutingError::NoProfiles);
            }

            // Extract features from the query profile
            let features = profile_to_features(_profile);
            let input_array = ndarray::Array2::from_shape_vec((1, NUM_FEATURES), features)
                .map_err(|e| RoutingError::AnalysisFailed(format!("failed to build input tensor: {e}")))?;

            // Run inference
            let outputs = self
                .session
                .run(
                    ort::inputs![input_array]
                        .map_err(|e| RoutingError::AnalysisFailed(format!("failed to prepare ONNX inputs: {e}")))?,
                )
                .map_err(|e| RoutingError::AnalysisFailed(format!("ONNX inference failed: {e}")))?;

            // Extract output probability tensor
            let output_tensor = outputs
                .first()
                .ok_or_else(|| RoutingError::AnalysisFailed("ONNX model returned no outputs".to_owned()))?
                .1
                .try_extract_tensor::<f32>()
                .map_err(|e| RoutingError::AnalysisFailed(format!("failed to extract output tensor: {e}")))?;

            let probabilities: Vec<f32> = output_tensor.iter().copied().collect();

            match select_model_from_probabilities(&probabilities, profiles, _profile) {
                Some(selected) => {
                    let alternatives = profiles
                        .iter()
                        .filter(|p| p.provider != selected.provider || p.model != selected.model)
                        .map(|p| (p.provider.clone(), p.model.clone()))
                        .collect();

                    tracing::debug!(
                        selected_model = %selected.id(),
                        num_classes = probabilities.len(),
                        num_profiles = profiles.len(),
                        "ONNX classifier selected model"
                    );

                    return Ok(RoutingDecision {
                        provider: selected.provider.clone(),
                        model: selected.model.clone(),
                        reason: crate::RoutingReason::OnnxClassified,
                        alternatives,
                    });
                }
                None => {
                    // Fallback to best quality when ONNX selection fails
                    tracing::warn!("ONNX model selection failed, falling back to best quality");

                    let best = registry.best_quality().ok_or(RoutingError::NoProfiles)?;

                    let alternatives = profiles
                        .iter()
                        .filter(|p| p.provider != best.provider || p.model != best.model)
                        .map(|p| (p.provider.clone(), p.model.clone()))
                        .collect();

                    return Ok(RoutingDecision {
                        provider: best.provider.clone(),
                        model: best.model.clone(),
                        reason: crate::RoutingReason::BestQuality,
                        alternatives,
                    });
                }
            }
        }

        #[cfg(not(feature = "onnx"))]
        {
            let _ = registry;
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
/// 0. `estimated_input_tokens / 100_000.0` — normalized token count
/// 1. `task_type` ordinal (0–5)
/// 2. `complexity` ordinal (0–2)
/// 3. `requires_tool_use` (0.0 or 1.0)
/// 4. `vision` capability (0.0 or 1.0)
/// 5. `long_context` capability (0.0 or 1.0)
/// 6. `message_count / 50.0` — normalized message count
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

/// Select a model from registry profiles using ONNX output probabilities
///
/// Performs argmax over the probability vector, mapping the winning index
/// to a registry profile. If the output dimension does not match the number
/// of profiles, or the selected model lacks required capabilities, falls back
/// to the highest-probability model that satisfies capability requirements.
/// Returns `None` when no valid selection can be made
#[cfg(feature = "onnx")]
fn select_model_from_probabilities(
    probabilities: &[f32],
    profiles: &[crate::registry::ModelProfile],
    query: &QueryProfile,
) -> Option<crate::registry::ModelProfile> {
    if probabilities.is_empty() || profiles.is_empty() {
        return None;
    }

    // Sort class indices by descending probability
    let mut ranked: Vec<(usize, f32)> = probabilities.iter().copied().enumerate().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Walk ranked list and return the first profile that satisfies capabilities
    for (idx, _prob) in &ranked {
        if *idx >= profiles.len() {
            continue;
        }

        let candidate = &profiles[*idx];

        if satisfies_capabilities(candidate, query) {
            return Some(candidate.clone());
        }
    }

    // No ranked candidate satisfies capabilities — return None so the
    // caller can fall back to best quality
    None
}

/// Check whether a model profile satisfies the query's required capabilities
#[cfg(feature = "onnx")]
fn satisfies_capabilities(profile: &crate::registry::ModelProfile, query: &QueryProfile) -> bool {
    let caps = &query.required_capabilities;

    if caps.tool_calling && !profile.tool_calling {
        return false;
    }
    if caps.vision && !profile.vision {
        return false;
    }
    if caps.long_context && !profile.long_context {
        return false;
    }

    true
}
