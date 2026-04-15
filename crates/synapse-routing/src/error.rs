//! Routing-specific error types

use thiserror::Error;

/// Errors that can occur during smart model routing
#[derive(Debug, Error)]
pub enum RoutingError {
    /// No model available for the requested routing class
    #[error("no model available for routing class: {class}")]
    NoModelAvailable { class: String },

    /// No model profiles configured for routing
    #[error("no model profiles configured for smart routing")]
    NoProfiles,

    /// Query analysis failed
    #[error("query analysis failed: {0}")]
    AnalysisFailed(String),

    /// All providers are currently unhealthy
    #[error("all providers are currently down")]
    AllProvidersDown,

    /// Feature not available in this build
    #[error("feature not available: {feature}")]
    FeatureNotAvailable { feature: String },
}
