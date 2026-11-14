use serde::{Deserialize, Serialize};

use super::message::Message;
use super::tool::{ToolChoice, ToolDefinition};

/// Parameters controlling text generation
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompletionParams {
    /// Sampling temperature (0.0 to 2.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Nucleus sampling threshold
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Maximum tokens to generate
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Stop sequences
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// Frequency penalty (-2.0 to 2.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    /// Presence penalty (-2.0 to 2.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    /// Random seed for deterministic generation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// How much reasoning effort the model spends (`low`, `medium`, `high`,
    /// ...): the `OpenAI` `reasoning_effort` or the Anthropic
    /// `output_config.effort`, mapped to whichever the upstream speaks
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Anthropic `thinking` configuration, passed through unchanged to
    /// Anthropic upstreams
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<serde_json::Value>,
    /// Anthropic `output_config` (e.g. `format`, `task_budget`), passed
    /// through to Anthropic upstreams with `effort` taken from
    /// `reasoning_effort`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_config: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Internal canonical completion request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    /// Model identifier
    pub model: String,
    /// Conversation messages
    pub messages: Vec<Message>,
    /// Generation parameters
    #[serde(default)]
    pub params: CompletionParams,
    /// Tool definitions available to the model
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    /// How the model should select tools
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// Whether to stream the response
    #[serde(default)]
    pub stream: bool,
}
