use serde::Deserialize;

/// CSRF protection configuration
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CsrfConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_header_name")]
    pub header_name: String,
}

impl Default for CsrfConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            header_name: "X-Synapse-CSRF-Protection".to_string(),
        }
    }
}

#[allow(clippy::missing_const_for_fn)]
fn default_enabled() -> bool {
    true
}

fn default_header_name() -> String {
    "X-Synapse-CSRF-Protection".to_string()
}
