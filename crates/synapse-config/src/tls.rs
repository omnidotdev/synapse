use serde::Deserialize;

/// TLS configuration for HTTPS
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// Path to the TLS certificate file
    pub certificate: String,
    /// Path to the TLS private key file
    pub private_key: String,
}
