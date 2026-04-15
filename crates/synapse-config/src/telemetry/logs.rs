use serde::Deserialize;

use super::exporters::ExporterConfig;

/// Logs configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogsConfig {
    /// Override the default exporter for logs
    #[serde(default)]
    pub exporter: Option<ExporterConfig>,
}
