use std::collections::HashMap;

use serde::Deserialize;
use url::Url;

/// OTLP exporter configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExporterConfig {
    /// OTLP endpoint URL
    pub endpoint: Url,
    /// Export protocol
    #[serde(default)]
    pub protocol: ExportProtocol,
    /// Additional headers for the exporter
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Batch export configuration
    #[serde(default)]
    pub batch: Option<BatchConfig>,
    /// TLS configuration
    #[serde(default)]
    pub tls: Option<ExporterTlsConfig>,
}

/// OTLP export protocol
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportProtocol {
    /// gRPC (default)
    #[default]
    Grpc,
    /// HTTP/protobuf
    HttpProto,
}

/// Batch export configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchConfig {
    /// Maximum batch size
    #[serde(default = "default_batch_size")]
    pub max_export_batch_size: usize,
    /// Maximum queue size
    #[serde(default = "default_queue_size")]
    pub max_queue_size: usize,
    /// Export interval in seconds
    #[serde(default = "default_export_interval")]
    pub scheduled_delay: u64,
}

/// TLS configuration for the exporter
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExporterTlsConfig {
    /// CA certificate path
    #[serde(default)]
    pub ca_cert: Option<String>,
    /// Client certificate path
    #[serde(default)]
    pub client_cert: Option<String>,
    /// Client key path
    #[serde(default)]
    pub client_key: Option<String>,
}

#[allow(clippy::missing_const_for_fn)]
fn default_batch_size() -> usize {
    512
}
#[allow(clippy::missing_const_for_fn)]
fn default_queue_size() -> usize {
    2048
}
#[allow(clippy::missing_const_for_fn)]
fn default_export_interval() -> u64 {
    5
}
