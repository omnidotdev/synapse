//! `OpenAI` chat completion API wire format types

use serde::{Deserialize, Serialize};

// -- Request types --

/// `OpenAI` chat completion request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiRequest {
    /// Model identifier
    pub model: String,
    /// Conversation messages
    pub messages: Vec<OpenAiMessage>,
    /// Sampling temperature
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
    /// Frequency penalty
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    /// Presence penalty
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    /// Random seed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Whether to stream the response
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Tool definitions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<OpenAiTool>>,
    /// Tool choice configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// Stream options (e.g. `include_usage`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<OpenAiStreamOptions>,
}

/// `OpenAI` stream options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiStreamOptions {
    /// Include usage statistics in stream
    #[serde(default)]
    pub include_usage: bool,
}

/// `OpenAI` message within a request or response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiMessage {
    /// Message role
    pub role: String,
    /// Content (string or array of content parts)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<OpenAiContent>,
    /// Participant name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Tool calls made by the assistant
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAiToolCall>>,
    /// Tool call ID this message responds to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// `OpenAI` content can be a string or array of content parts
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpenAiContent {
    /// Plain text content
    Text(String),
    /// Array of content parts
    Parts(Vec<OpenAiContentPart>),
}

/// Individual content part in an `OpenAI` message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenAiContentPart {
    /// Text content
    Text {
        /// The text string
        text: String,
    },
    /// Image content via URL
    ImageUrl {
        /// Image URL specification
        image_url: OpenAiImageUrl,
    },
}

/// Image URL specification for `OpenAI`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiImageUrl {
    /// Image URL or base64 data URI
    pub url: String,
    /// Detail level
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `OpenAI` tool definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiTool {
    /// Tool type (always "function")
    #[serde(rename = "type")]
    pub tool_type: String,
    /// Function specification
    pub function: OpenAiFunction,
}

/// `OpenAI` function specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiFunction {
    /// Function name
    pub name: String,
    /// Human-readable description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for parameters
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

/// `OpenAI` tool call within a message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiToolCall {
    /// Unique tool call identifier
    pub id: String,
    /// Tool type (always "function")
    #[serde(rename = "type")]
    pub tool_type: String,
    /// Function call details
    pub function: OpenAiFunctionCall,
}

/// Function call details within an `OpenAI` tool call
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiFunctionCall {
    /// Function name
    pub name: String,
    /// JSON-encoded arguments
    pub arguments: String,
}

// -- Response types --

/// `OpenAI` chat completion response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiResponse {
    /// Response identifier
    pub id: String,
    /// Object type
    pub object: String,
    /// Creation timestamp
    pub created: u64,
    /// Model used
    pub model: String,
    /// Generated choices
    pub choices: Vec<OpenAiChoice>,
    /// Token usage
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<OpenAiUsage>,
}

/// Choice within an `OpenAI` response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiChoice {
    /// Choice index
    pub index: u32,
    /// Generated message
    pub message: OpenAiChoiceMessage,
    /// Why generation stopped
    pub finish_reason: Option<String>,
}

/// Message within an `OpenAI` response choice
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiChoiceMessage {
    /// Role (always "assistant")
    pub role: String,
    /// Text content
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Tool calls
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAiToolCall>>,
}

/// Token usage in an `OpenAI` response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiUsage {
    /// Prompt tokens
    pub prompt_tokens: u32,
    /// Completion tokens
    pub completion_tokens: u32,
    /// Total tokens
    pub total_tokens: u32,
}

// -- Streaming types --

/// `OpenAI` streaming chunk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiStreamChunk {
    /// Chunk identifier
    pub id: String,
    /// Object type (always "chat.completion.chunk")
    pub object: String,
    /// Creation timestamp
    pub created: u64,
    /// Model used
    pub model: String,
    /// Delta choices
    pub choices: Vec<OpenAiStreamChoice>,
    /// Usage (present on final chunk when `stream_options.include_usage` is true)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<OpenAiUsage>,
}

/// Choice within a streaming chunk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiStreamChoice {
    /// Choice index
    pub index: u32,
    /// Incremental delta
    pub delta: OpenAiStreamDelta,
    /// Finish reason (present on final chunk)
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// Delta content within a streaming choice
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiStreamDelta {
    /// Role (present on first chunk)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Incremental text content
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Incremental tool calls
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAiStreamToolCall>>,
}

/// Tool call within a streaming delta
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiStreamToolCall {
    /// Index within the `tool_calls` array
    pub index: u32,
    /// Tool call ID (first chunk only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Tool type (first chunk only)
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub tool_type: Option<String>,
    /// Partial function call
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<OpenAiStreamFunctionCall>,
}

/// Partial function call within a streaming tool call
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiStreamFunctionCall {
    /// Function name (first chunk only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Incremental arguments fragment
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

// -- Models list types --

/// `OpenAI` models list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiModelList {
    /// Object type
    pub object: String,
    /// List of models
    pub data: Vec<OpenAiModel>,
}

/// `OpenAI` model entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiModel {
    /// Model identifier
    pub id: String,
    /// Object type (always "model")
    pub object: String,
    /// Creation timestamp
    #[serde(default)]
    pub created: u64,
    /// Owner
    #[serde(default)]
    pub owned_by: String,
}

// -- Error response --

/// `OpenAI` error response body
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiErrorResponse {
    /// Error details
    pub error: OpenAiErrorDetail,
}

/// `OpenAI` error detail
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiErrorDetail {
    /// Error message
    pub message: String,
    /// Error type
    #[serde(rename = "type")]
    pub error_type: String,
    /// Parameter that caused the error
    #[serde(default)]
    pub param: Option<String>,
    /// Error code
    #[serde(default)]
    pub code: Option<String>,
}
