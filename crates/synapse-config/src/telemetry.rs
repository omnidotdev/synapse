pub mod exporters;
pub mod logs;
pub mod metrics;
pub mod tracing;

use std::collections::HashMap;

use serde::Deserialize;

use self::{exporters::ExporterConfig, logs::LogsConfig, metrics::MetricsConfig, tracing::TracingConfig};

/// Telemetry configuration
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    /// Service name for telemetry metadata
    #[serde(default = "default_service_name")]
    pub service_name: String,
    /// Additional resource attributes
    #[serde(default)]
    pub resource_attributes: HashMap<String, String>,
    /// Default exporter configuration (shared by tracing, metrics, logs)
    #[serde(default)]
    pub exporter: Option<ExporterConfig>,
    /// Tracing-specific configuration
    #[serde(default)]
    pub tracing: Option<TracingConfig>,
    /// Metrics-specific configuration
    #[serde(default)]
    pub metrics: Option<MetricsConfig>,
    /// Logs-specific configuration
    #[serde(default)]
    pub logs: Option<LogsConfig>,
}

fn default_service_name() -> String {
    "synapse".to_string()
}
