use serde::{Deserialize, Serialize};

// -- Chat completion request types --

/// Chat completion request (OpenAI-compatible)
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    /// Model identifier
    pub model: String,
    /// Conversation messages
    pub messages: Vec<Message>,
    /// Whether to stream the response
    #[serde(default)]
    pub stream: bool,
    /// Sampling temperature (0.0 to 2.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Nucleus sampling threshold
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Maximum tokens to generate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Stop sequences
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// Tool definitions available to the model
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    /// How the model should select tools
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
}

/// Message in a conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Role of the message author
    pub role: String,
    /// Message content (text or structured parts)
    pub content: serde_json::Value,
    /// Tool calls made by the assistant
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// ID of the tool call this message responds to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    /// Create a system message
    pub fn system(content: &str) -> Self {
        Self {
            role: "system".to_owned(),
            content: serde_json::Value::String(content.to_owned()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Create a user message
    pub fn user(content: &str) -> Self {
        Self {
            role: "user".to_owned(),
            content: serde_json::Value::String(content.to_owned()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Create an assistant message
    pub fn assistant(content: &str) -> Self {
        Self {
            role: "assistant".to_owned(),
            content: serde_json::Value::String(content.to_owned()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Create a tool result message
    pub fn tool(tool_call_id: &str, content: &str) -> Self {
        Self {
            role: "tool".to_owned(),
            content: serde_json::Value::String(content.to_owned()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_owned()),
        }
    }
}

// -- Chat completion response types --

/// Chat completion response
#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    /// Unique response identifier
    pub id: String,
    /// Object type
    pub object: String,
    /// Unix timestamp
    pub created: u64,
    /// Model used
    pub model: String,
    /// Generated choices
    pub choices: Vec<Choice>,
    /// Token usage statistics
    pub usage: Option<Usage>,
}

/// A single completion choice
#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    /// Choice index
    pub index: u32,
    /// Generated message
    pub message: ChoiceMessage,
    /// Why generation stopped
    pub finish_reason: Option<String>,
}

/// Message in a response choice
#[derive(Debug, Clone, Deserialize)]
pub struct ChoiceMessage {
    /// Role (always "assistant")
    pub role: String,
    /// Text content
    pub content: Option<String>,
    /// Tool calls requested by the model
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

/// Token usage statistics
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Usage {
    /// Tokens consumed by the prompt
    pub prompt_tokens: u32,
    /// Tokens generated in the completion
    pub completion_tokens: u32,
    /// Total tokens
    pub total_tokens: u32,
}

// -- Tool types --

/// A tool call requested by the model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique identifier
    pub id: String,
    /// Tool type (always "function")
    #[serde(rename = "type", default = "default_tool_type")]
    pub tool_type: String,
    /// Function details
    pub function: FunctionCall,
}

fn default_tool_type() -> String {
    "function".to_owned()
}

/// Function name and arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    /// Function name
    pub name: String,
    /// JSON-encoded arguments
    pub arguments: String,
}

/// Tool definition for the model
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    /// Tool type (always "function")
    #[serde(rename = "type")]
    pub tool_type: String,
    /// Function specification
    pub function: FunctionDefinition,
}

/// Function specification
#[derive(Debug, Clone, Serialize)]
pub struct FunctionDefinition {
    /// Function name
    pub name: String,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

// -- Streaming types --

/// SSE streaming chunk (`OpenAI` format)
#[derive(Debug, Clone, Deserialize)]
pub struct StreamChunk {
    /// Chunk identifier
    pub id: String,
    /// Object type
    pub object: String,
    /// Creation timestamp
    pub created: u64,
    /// Model used
    pub model: String,
    /// Delta choices
    pub choices: Vec<StreamChoice>,
    /// Usage (on final chunk)
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// Choice within a streaming chunk
#[derive(Debug, Clone, Deserialize)]
pub struct StreamChoice {
    /// Choice index
    pub index: u32,
    /// Delta content
    pub delta: StreamDelta,
    /// Finish reason (on final chunk)
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// Delta content within a streaming choice
#[derive(Debug, Clone, Deserialize)]
pub struct StreamDelta {
    /// Role (first chunk only)
    #[serde(default)]
    pub role: Option<String>,
    /// Incremental text content
    #[serde(default)]
    pub content: Option<String>,
    /// Incremental tool calls
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamToolCall>>,
}

/// Tool call within a streaming delta
#[derive(Debug, Clone, Deserialize)]
pub struct StreamToolCall {
    /// Index in the `tool_calls` array
    pub index: u32,
    /// Tool call ID (first chunk only)
    #[serde(default)]
    pub id: Option<String>,
    /// Partial function call
    #[serde(default)]
    pub function: Option<StreamFunctionCall>,
}

/// Partial function call in a streaming tool call
#[derive(Debug, Clone, Deserialize)]
pub struct StreamFunctionCall {
    /// Function name (first chunk only)
    #[serde(default)]
    pub name: Option<String>,
    /// Incremental arguments fragment
    #[serde(default)]
    pub arguments: Option<String>,
}

/// High-level streaming event parsed from SSE
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// Incremental text content
    ContentDelta(String),
    /// Start of a tool call
    ToolCallStart {
        /// Index in the `tool_calls` array
        index: u32,
        /// Tool call ID
        id: String,
        /// Function name
        name: String,
    },
    /// Incremental tool call arguments
    ToolCallDelta {
        /// Index in the `tool_calls` array
        index: u32,
        /// Arguments fragment
        arguments: String,
    },
    /// Stream finished
    Done {
        /// Finish reason
        finish_reason: Option<String>,
        /// Usage statistics
        usage: Option<Usage>,
    },
    /// Error during streaming
    Error(String),
}

// -- Model types --

/// Model list response
#[derive(Debug, Clone, Deserialize)]
pub struct ModelList {
    /// Object type
    pub object: String,
    /// Available models
    pub data: Vec<Model>,
}

/// A model entry
#[derive(Debug, Clone, Deserialize)]
pub struct Model {
    /// Model identifier
    pub id: String,
    /// Object type
    pub object: String,
    /// Creation timestamp
    #[serde(default)]
    pub created: u64,
    /// Owner
    #[serde(default)]
    pub owned_by: String,
}

// -- MCP types --

/// Tool info from MCP listing
#[derive(Debug, Clone, Deserialize)]
pub struct McpTool {
    /// Qualified tool name
    pub name: String,
    /// Server that provides the tool
    pub server: String,
    /// Tool description
    pub description: String,
    /// JSON Schema for input
    pub input_schema: serde_json::Value,
}

/// Result from a tool call
#[derive(Debug, Clone, Deserialize)]
pub struct ToolResult {
    /// Content blocks
    pub content: Vec<ContentBlock>,
    /// Whether the result is an error
    #[serde(default)]
    pub is_error: Option<bool>,
}

impl ToolResult {
    /// Concatenate text content blocks into a single string
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Content block in a tool result
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Text content
    Text {
        /// The text
        text: String,
    },
    /// Image content
    Image {
        /// Base64-encoded image data
        data: String,
        /// MIME type
        mime_type: String,
    },
}

impl From<McpTool> for ToolDefinition {
    fn from(t: McpTool) -> Self {
        Self {
            tool_type: "function".to_owned(),
            function: FunctionDefinition {
                name: t.name,
                description: Some(t.description),
                parameters: Some(t.input_schema),
            },
        }
    }
}

/// Tool search result
#[derive(Debug, Clone, Deserialize)]
pub struct ToolSearchResult {
    /// Qualified tool name
    pub qualified_name: String,
    /// Server name
    pub server_name: String,
    /// Original tool name
    pub tool_name: String,
    /// Tool description
    pub description: String,
    /// Relevance score
    pub score: f32,
}

// -- STT types --

/// Transcription response
#[derive(Debug, Clone, Deserialize)]
pub struct Transcription {
    /// Transcribed text
    pub text: String,
}

// -- TTS types --

/// Speech synthesis request
#[derive(Debug, Clone, Serialize)]
pub struct SpeechRequest {
    /// Model identifier
    pub model: String,
    /// Text to synthesize
    pub input: String,
    /// Voice identifier
    pub voice: String,
    /// Output format
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<String>,
    /// Speed multiplier (0.25 to 4.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
}

// -- Embeddings types --

/// Embedding request
#[derive(Debug, Clone, Serialize)]
pub struct EmbedRequest {
    /// Input text(s) to embed
    pub input: EmbedInput,
    /// Model identifier
    pub model: String,
    /// Number of dimensions (optional, model-dependent)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<u32>,
}

/// Embedding input: single string or array
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum EmbedInput {
    /// Single text input
    Single(String),
    /// Multiple text inputs
    Multiple(Vec<String>),
}

/// Embedding response
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingResponse {
    /// Object type (always "list")
    pub object: String,
    /// Embedding results
    pub data: Vec<EmbeddingData>,
    /// Model used
    pub model: String,
    /// Token usage
    pub usage: EmbeddingUsage,
}

/// Single embedding entry
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingData {
    /// Object type (always "embedding")
    pub object: String,
    /// The embedding vector
    pub embedding: Vec<f32>,
    /// Index in the input array
    pub index: usize,
}

/// Token usage for embeddings
#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingUsage {
    /// Tokens in the input
    pub prompt_tokens: u32,
    /// Total tokens used
    pub total_tokens: u32,
}

// -- Image generation types --

/// Image generation request
#[derive(Debug, Clone, Serialize)]
pub struct ImageRequest {
    /// Text description of the desired image
    pub prompt: String,
    /// Model identifier
    pub model: String,
    /// Size of generated images
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    /// Quality level
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    /// Number of images to generate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    /// Response format ("url" or "`b64_json`")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<String>,
}

/// Image generation response
#[derive(Debug, Clone, Deserialize)]
pub struct ImageResponse {
    /// Unix timestamp
    pub created: u64,
    /// Generated image data
    pub data: Vec<ImageData>,
}

/// Single generated image
#[derive(Debug, Clone, Deserialize)]
pub struct ImageData {
    /// URL of the generated image
    #[serde(default)]
    pub url: Option<String>,
    /// Base64-encoded image data
    #[serde(default)]
    pub b64_json: Option<String>,
    /// Revised prompt (DALL-E 3)
    #[serde(default)]
    pub revised_prompt: Option<String>,
}
