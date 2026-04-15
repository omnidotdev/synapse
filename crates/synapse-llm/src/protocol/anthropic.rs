//! Anthropic Messages API wire format types

use serde::{Deserialize, Serialize};

// -- Request types --

/// Anthropic messages API request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicRequest {
    /// Model identifier
    pub model: String,
    /// Maximum tokens to generate (required by Anthropic)
    pub max_tokens: u32,
    /// System prompt (top-level, not in messages)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Conversation messages
    pub messages: Vec<AnthropicMessage>,
    /// Sampling temperature
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Nucleus sampling threshold
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Top-k sampling
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Stop sequences
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    /// Whether to stream the response
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Tool definitions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
    /// Tool choice configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<AnthropicToolChoice>,
}

/// Anthropic message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    /// Role ("user" or "assistant")
    pub role: String,
    /// Content blocks
    pub content: AnthropicContent,
}

/// Anthropic content can be a string or array of content blocks
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnthropicContent {
    /// Plain text (shorthand)
    Text(String),
    /// Array of content blocks
    Blocks(Vec<AnthropicContentBlock>),
}

/// Content block in an Anthropic message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContentBlock {
    /// Text content
    Text {
        /// The text string
        text: String,
    },
    /// Image content
    Image {
        /// Image source
        source: AnthropicImageSource,
    },
    /// Tool use request from the assistant
    ToolUse {
        /// Tool use identifier
        id: String,
        /// Tool name
        name: String,
        /// Tool input as JSON
        input: serde_json::Value,
    },
    /// Tool result from the user
    ToolResult {
        /// Tool use ID this result responds to
        tool_use_id: String,
        /// Result content
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        /// Whether the tool call errored
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

/// Anthropic image source
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicImageSource {
    /// Source type (e.g. "base64", "url")
    #[serde(rename = "type")]
    pub source_type: String,
    /// Media type (e.g. "image/png")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Image data (base64 encoded) or URL
    pub data: String,
}

/// Anthropic tool definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicTool {
    /// Tool name
    pub name: String,
    /// Human-readable description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for input parameters
    pub input_schema: serde_json::Value,
}

/// Anthropic tool choice
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicToolChoice {
    /// Choice type: "auto", "any", or "tool"
    #[serde(rename = "type")]
    pub choice_type: String,
    /// Specific tool name (when type is "tool")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

// -- Response types --

/// Anthropic messages API response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicResponse {
    /// Response identifier
    pub id: String,
    /// Object type (always "message")
    #[serde(rename = "type")]
    pub response_type: String,
    /// Role (always "assistant")
    pub role: String,
    /// Response content blocks
    pub content: Vec<AnthropicResponseBlock>,
    /// Model used
    pub model: String,
    /// Stop reason
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Stop sequence that triggered the stop
    #[serde(default)]
    pub stop_sequence: Option<String>,
    /// Token usage
    pub usage: AnthropicUsage,
}

/// Content block in an Anthropic response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicResponseBlock {
    /// Text response
    Text {
        /// The text string
        text: String,
    },
    /// Tool use request
    ToolUse {
        /// Tool use identifier
        id: String,
        /// Tool name
        name: String,
        /// Tool input as JSON
        input: serde_json::Value,
    },
}

/// Anthropic token usage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicUsage {
    /// Input tokens
    pub input_tokens: u32,
    /// Output tokens
    pub output_tokens: u32,
}

// -- Streaming types --

/// Anthropic SSE event types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicStreamEvent {
    /// Stream started
    MessageStart {
        /// Partial message with metadata
        message: AnthropicStreamMessage,
    },
    /// New content block started
    ContentBlockStart {
        /// Block index
        index: u32,
        /// Initial block content
        content_block: AnthropicStreamContentBlock,
    },
    /// Incremental content within a block
    ContentBlockDelta {
        /// Block index
        index: u32,
        /// Delta content
        delta: AnthropicStreamDelta,
    },
    /// Content block finished
    ContentBlockStop {
        /// Block index
        index: u32,
    },
    /// Message metadata delta (stop reason, usage)
    MessageDelta {
        /// Delta with stop reason
        delta: AnthropicMessageDelta,
        /// Updated usage
        #[serde(default)]
        usage: Option<AnthropicUsage>,
    },
    /// Stream completed
    MessageStop,
    /// Ping event for keep-alive
    Ping,
}

/// Partial message in a `message_start` event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicStreamMessage {
    /// Response identifier
    pub id: String,
    /// Object type
    #[serde(rename = "type")]
    pub message_type: String,
    /// Role
    pub role: String,
    /// Model
    pub model: String,
    /// Initial usage
    #[serde(default)]
    pub usage: Option<AnthropicUsage>,
}

/// Content block in a `content_block_start` event
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicStreamContentBlock {
    /// Text block
    Text {
        /// Initial text (usually empty)
        text: String,
    },
    /// Tool use block
    ToolUse {
        /// Tool use ID
        id: String,
        /// Tool name
        name: String,
        /// Initial input (usually empty object)
        input: serde_json::Value,
    },
}

/// Delta content in a `content_block_delta` event
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicStreamDelta {
    /// Incremental text
    TextDelta {
        /// Text fragment
        text: String,
    },
    /// Incremental tool input JSON
    InputJsonDelta {
        /// JSON fragment
        partial_json: String,
    },
}

/// Delta in a `message_delta` event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessageDelta {
    /// Stop reason
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Stop sequence
    #[serde(default)]
    pub stop_sequence: Option<String>,
}

// -- Error response --

/// Anthropic error response body
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicErrorResponse {
    /// Error type
    #[serde(rename = "type")]
    pub error_type: String,
    /// Error details
    pub error: AnthropicErrorDetail,
}

/// Anthropic error detail
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicErrorDetail {
    /// Error type
    #[serde(rename = "type")]
    pub error_type: String,
    /// Error message
    pub message: String,
}
