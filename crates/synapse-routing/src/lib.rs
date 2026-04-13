//! Smart model routing for Synapse
//!
//! Provides heuristic-based routing strategies adapted from `LLMRouter`:
//! - **Threshold**: route by complexity (`HybridLLMRouter` pattern)
//! - **Cost**: maximize quality within budget
//! - **Cascade**: try cheap first, escalate on low confidence (`AutomixRouter` pattern)

#![allow(
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

pub mod analysis;
pub mod error;
pub mod feedback;
pub mod registry;
pub mod scoring;
pub mod strategy;

pub use analysis::{AnalysisInput, QueryProfile, RequiredCapabilities, analyze_query, analyze_query_structured};
pub use error::RoutingError;
pub use feedback::{FeedbackTracker, ModelFeedback, RequestFeedback};
pub use registry::{ModelProfile, ModelRegistry};
pub use strategy::failover::FailoverState;
pub use strategy::{Strategy, StrategyRegistry};

/// The reason a particular model was selected
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingReason {
    /// Query classified as low complexity
    LowComplexity,
    /// Query classified as high complexity
    HighComplexity,
    /// Best quality model selected (no budget constraint)
    BestQuality,
    /// Model selected to fit within cost budget
    CostConstrained,
    /// Initial cheap model in cascade flow
    CascadeInitial,
    /// Escalated to stronger model after low-confidence response
    CascadeEscalated,
    /// Selected by multi-objective score optimization
    ScoreOptimized,
    /// Selected by ONNX ML classifier
    OnnxClassified,
}

/// Result of a routing decision
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    /// Selected provider name
    pub provider: String,
    /// Selected model identifier
    pub model: String,
    /// Why this model was selected
    pub reason: RoutingReason,
    /// Alternative (provider, model) pairs for failover
    pub alternatives: Vec<(String, String)>,
}

/// Route a request using the configured strategy
pub fn route_request(
    messages: &[serde_json::Value],
    has_tools: bool,
    registry: &ModelRegistry,
    config: &synapse_config::RoutingConfig,
    feedback: Option<&FeedbackTracker>,
) -> Result<RoutingDecision, RoutingError> {
    let message_count = messages.len();
    let has_system_prompt = messages
        .iter()
        .any(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"));

    let input = AnalysisInput {
        messages,
        has_tools,
        has_images: false,
        message_count,
        has_system_prompt,
        tool_call_turns: 0,
        is_multi_turn: false,
    };

    route_request_structured(&input, registry, config, feedback)
}

/// Route a request using the configured strategy with structured input
pub fn route_request_structured(
    input: &AnalysisInput,
    registry: &ModelRegistry,
    config: &synapse_config::RoutingConfig,
    feedback: Option<&FeedbackTracker>,
) -> Result<RoutingDecision, RoutingError> {
    let strategy_registry = StrategyRegistry::from_config(config);
    route_with_strategy_registry(input, registry, config, &strategy_registry, feedback)
}

/// Route a request using a pre-built strategy registry
///
/// Prefer this when you have a `StrategyRegistry` already (e.g. stored in
/// application state with custom strategies registered)
pub fn route_with_strategy_registry(
    input: &AnalysisInput,
    registry: &ModelRegistry,
    config: &synapse_config::RoutingConfig,
    strategy_registry: &StrategyRegistry,
    feedback: Option<&FeedbackTracker>,
) -> Result<RoutingDecision, RoutingError> {
    let profile = analyze_query_structured(input);

    tracing::debug!(
        task_type = ?profile.task_type,
        complexity = ?profile.complexity,
        tokens = profile.estimated_input_tokens,
        messages = profile.message_count,
        "query analyzed for routing"
    );

    // Filter models by required capabilities
    let filtered = registry.filtered(&profile.required_capabilities);

    if filtered.profiles().is_empty() {
        return Err(RoutingError::NoModelAvailable {
            class: format!("no model satisfies capabilities: {:?}", profile.required_capabilities),
        });
    }

    let strategy_name = StrategyRegistry::resolve_name(config);
    let strategy = strategy_registry
        .get(strategy_name)
        .ok_or_else(|| RoutingError::NoModelAvailable {
            class: format!("unknown strategy: {strategy_name}"),
        })?;

    let decision = strategy.route(&profile, &filtered, feedback)?;

    tracing::info!(
        provider = %decision.provider,
        model = %decision.model,
        reason = ?decision.reason,
        alternatives = decision.alternatives.len(),
        strategy = strategy_name,
        "routing decision made"
    );

    Ok(decision)
}
