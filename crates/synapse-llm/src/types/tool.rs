use serde::{Deserialize, Serialize};

/// Definition of a tool the model can call
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool type (currently always "function")
    #[serde(rename = "type")]
    pub tool_type: String,
    /// Function specification
    pub function: FunctionDefinition,
}

/// Specification of a callable function
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    /// Function name
    pub name: String,
    /// Human-readable description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the function parameters
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

/// How the model should select tools
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    /// Simple mode: "none", "auto", or "required"
    Mode(ToolChoiceMode),
    /// Force a specific function
    Function(ToolChoiceFunction),
}

/// Tool selection mode
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoiceMode {
    /// Model will not call any tools
    None,
    /// Model decides whether to call tools
    Auto,
    /// Model must call at least one tool
    Required,
}

/// Force the model to call a specific function
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolChoiceFunction {
    /// Must be "function"
    #[serde(rename = "type")]
    pub tool_type: String,
    /// Function to call
    pub function: ToolChoiceFunctionName,
}

/// Function name reference within a forced tool choice
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolChoiceFunctionName {
    /// Name of the function to call
    pub name: String,
}
