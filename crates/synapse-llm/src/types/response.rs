use serde::{Deserialize, Serialize};

use super::message::{FunctionCall, ToolCall};

/// Reason the model stopped generating
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Natural end of generation
    Stop,
    /// Hit the `max_tokens` limit
    Length,
    /// Model decided to call a tool
    ToolCalls,
    /// Content was filtered by safety systems
    ContentFilter,
}

/// Token usage statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens consumed by the prompt
    pub prompt_tokens: u32,
    /// Tokens generated in the completion
    pub completion_tokens: u32,
    /// Total tokens (prompt + completion)
    pub total_tokens: u32,
    /// What the request cost, set by the gateway once it is billed. Never
    /// stored in the response cache: a cache hit is costed afresh
    #[serde(skip)]
    pub cost: Option<RequestCost>,
}

/// What a request cost the caller, reported back on its usage
///
/// Serialized as the `synapse_cost` object on the final usage of a response
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct RequestCost {
    /// Retail amount charged in US dollars, after the product margin and
    /// rounded up to the ledger's $0.0001 like the charge itself (0 for BYOK)
    pub cost_usd: f64,
    /// `cost_usd` in display credits ($0.01 each)
    pub credits: f64,
    /// Upstream provider cost in US dollars, before the margin (0 for BYOK,
    /// whose upstream cost the caller pays directly)
    pub provider_cost_usd: f64,
}

impl RequestCost {
    /// The cost of a request charged `charged_usd` (already rounded to the
    /// ledger precision) for `provider_cost_usd` of upstream spend
    #[must_use]
    pub fn new(charged_usd: f64, provider_cost_usd: f64) -> Self {
        Self {
            cost_usd: charged_usd,
            credits: (charged_usd * 10_000.0).round() / 100.0,
            provider_cost_usd,
        }
    }

    /// The combined cost of two billed attempts at one request
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        Self::new(
            self.cost_usd + other.cost_usd,
            self.provider_cost_usd + other.provider_cost_usd,
        )
    }
}

/// A single completion choice
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    /// Index of this choice
    pub index: u32,
    /// Generated message
    pub message: ChoiceMessage,
    /// Why generation stopped
    pub finish_reason: Option<FinishReason>,
}

/// Message content within a response choice
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChoiceMessage {
    /// Role is always assistant for completions
    pub role: String,
    /// Text content
    pub content: Option<String>,
    /// Tool calls requested by the model
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl ChoiceMessage {
    /// Create a simple text message from the assistant
    pub fn text(content: String) -> Self {
        Self {
            role: "assistant".to_owned(),
            content: Some(content),
            tool_calls: None,
        }
    }

    /// Create a tool-calling message from the assistant
    pub fn with_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: "assistant".to_owned(),
            content: None,
            tool_calls: Some(tool_calls),
        }
    }
}

/// Internal canonical completion response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    /// Unique response identifier
    pub id: String,
    /// Object type (e.g. "chat.completion")
    pub object: String,
    /// Unix timestamp of creation
    pub created: u64,
    /// Model used for generation
    pub model: String,
    /// Generated choices
    pub choices: Vec<Choice>,
    /// Token usage statistics
    pub usage: Option<Usage>,
}

/// Build a tool call from raw parts
pub const fn build_tool_call(id: String, name: String, arguments: String) -> ToolCall {
    ToolCall {
        id,
        function: FunctionCall { name, arguments },
    }
}
