//! Anthropic Messages API wire format types

use serde::{Deserialize, Serialize};

use crate::types::CacheControl;

// -- Request types --

/// Anthropic messages API request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicRequest {
    /// Model identifier
    pub model: String,
    /// Maximum tokens to generate (required by Anthropic)
    pub max_tokens: u32,
    /// System prompt (top-level, not in messages). Accepts a plain string or an array of
    /// content blocks (used by clients that attach `cache_control` to individual blocks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<AnthropicSystemPrompt>,
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

/// System prompt shape — string shorthand or array of content blocks
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnthropicSystemPrompt {
    /// Plain text (shorthand)
    Text(String),
    /// Array of content blocks (typically text with optional cache_control)
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
        /// Optional prompt-caching annotation (preserved across the proxy boundary
        /// so upstream Anthropic can apply caching to the marked prefix).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_accepts_plain_string() {
        let json = r#"{
            "model": "claude-sonnet-4",
            "max_tokens": 1024,
            "system": "You are a helpful assistant.",
            "messages": []
        }"#;
        let req: AnthropicRequest = serde_json::from_str(json).expect("deserialize");
        match req.system {
            Some(AnthropicSystemPrompt::Text(text)) => assert_eq!(text, "You are a helpful assistant."),
            other => panic!("expected Text variant, got {other:?}"),
        }
    }

    #[test]
    fn system_accepts_content_block_array() {
        let json = r#"{
            "model": "claude-sonnet-4",
            "max_tokens": 1024,
            "system": [
                { "type": "text", "text": "First block." },
                { "type": "text", "text": "Second block." }
            ],
            "messages": []
        }"#;
        let req: AnthropicRequest = serde_json::from_str(json).expect("deserialize");
        match req.system {
            Some(AnthropicSystemPrompt::Blocks(blocks)) => {
                assert_eq!(blocks.len(), 2);
                match &blocks[0] {
                    AnthropicContentBlock::Text { text, .. } => assert_eq!(text, "First block."),
                    other => panic!("expected text block, got {other:?}"),
                }
            }
            other => panic!("expected Blocks variant, got {other:?}"),
        }
    }

    #[test]
    fn system_block_preserves_cache_control_on_deserialize() {
        // pi-ai attaches `cache_control` to system blocks to trigger upstream prompt
        // caching. The field must deserialize into the typed `CacheControl` value so it
        // survives the wire-format → internal → wire-format round trip back to Anthropic.
        let json = r#"{
            "model": "claude-sonnet-4",
            "max_tokens": 1024,
            "system": [
                {
                    "type": "text",
                    "text": "Cached system prompt.",
                    "cache_control": { "type": "ephemeral" }
                }
            ],
            "messages": []
        }"#;
        let req: AnthropicRequest = serde_json::from_str(json).expect("deserialize");
        match req.system {
            Some(AnthropicSystemPrompt::Blocks(blocks)) => {
                assert_eq!(blocks.len(), 1);
                match &blocks[0] {
                    AnthropicContentBlock::Text { text, cache_control } => {
                        assert_eq!(text, "Cached system prompt.");
                        let cc = cache_control.as_ref().expect("cache_control preserved");
                        assert_eq!(cc.cache_type, "ephemeral");
                    }
                    other => panic!("expected text block, got {other:?}"),
                }
            }
            other => panic!("expected Blocks variant, got {other:?}"),
        }
    }

    #[test]
    fn system_omitted_remains_none() {
        let json = r#"{
            "model": "claude-sonnet-4",
            "max_tokens": 1024,
            "messages": []
        }"#;
        let req: AnthropicRequest = serde_json::from_str(json).expect("deserialize");
        assert!(req.system.is_none());
    }
}
