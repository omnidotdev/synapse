use serde::Deserialize;

use super::exporters::ExporterConfig;

/// Tracing configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TracingConfig {
    /// Sampling rate (0.0 to 1.0)
    #[serde(default = "default_sampling_rate")]
    pub sampling_rate: f64,
    /// Use parent-based sampler
    #[serde(default = "default_true")]
    pub parent_based: bool,
    /// Propagation formats
    #[serde(default)]
    pub propagation: Vec<PropagationFormat>,
    /// Collection limits
    #[serde(default)]
    pub limits: Option<TracingLimits>,
    /// Override the default exporter for tracing
    #[serde(default)]
    pub exporter: Option<ExporterConfig>,
}

/// Trace context propagation format
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PropagationFormat {
    /// W3C Trace Context
    TraceContext,
    /// AWS X-Ray
    Xray,
}

/// Collection limits for tracing
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TracingLimits {
    /// Maximum events per span
    #[serde(default)]
    pub max_events_per_span: Option<u32>,
    /// Maximum attributes per span
    #[serde(default)]
    pub max_attributes_per_span: Option<u32>,
    /// Maximum links per span
    #[serde(default)]
    pub max_links_per_span: Option<u32>,
}

#[allow(clippy::missing_const_for_fn)]
fn default_sampling_rate() -> f64 {
    1.0
}
#[allow(clippy::missing_const_for_fn)]
fn default_true() -> bool {
    true
}
