use std::net::SocketAddr;

use serde::Deserialize;

/// Health check endpoint configuration
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub listen_address: Option<SocketAddr>,
    #[serde(default = "default_path")]
    pub path: String,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen_address: None,
            path: "/health".to_string(),
        }
    }
}

#[allow(clippy::missing_const_for_fn)]
fn default_enabled() -> bool {
    true
}

fn default_path() -> String {
    "/health".to_string()
}
