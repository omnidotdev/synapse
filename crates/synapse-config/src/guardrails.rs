use serde::Deserialize;

/// Guardrails configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardrailsConfig {
    /// Whether guardrails are enabled
    #[serde(default)]
    pub enabled: bool,
    /// Check input content (before routing to provider)
    #[serde(default = "default_true")]
    pub check_input: bool,
    /// Check output content (after provider response)
    #[serde(default)]
    pub check_output: bool,
    /// Rules to evaluate
    #[serde(default)]
    pub rules: Vec<synapse_guardrails::Rule>,
}

const fn default_true() -> bool {
    true
}
