use serde::Deserialize;

use super::exporters::ExporterConfig;

/// Metrics configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsConfig {
    /// Override the default exporter for metrics
    #[serde(default)]
    pub exporter: Option<ExporterConfig>,
}
