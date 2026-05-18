use serde::{Deserialize, Serialize};

use super::message::FunctionCall;
use super::response::{FinishReason, Usage};

/// Server-sent event during streaming
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamEvent {
    /// Incremental content delta
    Delta(StreamDelta),
    /// Final usage statistics (sent at stream end)
    Usage(Usage),
    /// Stream has completed
    Done,
}

/// Incremental update within a streaming response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamDelta {
    /// Choice index this delta belongs to
    pub index: u32,
    /// Incremental text content
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Incremental tool call data
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<StreamToolCall>,
    /// Reason generation finished (present on final delta)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
}

/// Partial tool call data within a stream delta
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamToolCall {
    /// Index of this tool call in the `tool_calls` array
    pub index: u32,
    /// Tool call ID (present on first chunk only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Partial function call data
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<StreamFunctionCall>,
}

/// Partial function call data within a streaming tool call
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamFunctionCall {
    /// Function name (present on first chunk only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Incremental arguments JSON fragment
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

impl StreamFunctionCall {
    /// Convert to a complete `FunctionCall` if both name and arguments are present
    pub fn into_function_call(self) -> Option<FunctionCall> {
        match (self.name, self.arguments) {
            (Some(name), Some(arguments)) => Some(FunctionCall { name, arguments }),
            _ => None,
        }
    }
}
