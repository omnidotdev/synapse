//! Google Generative Language API wire format types

use serde::{Deserialize, Serialize};

// -- Request types --

/// Google `generateContent` request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleRequest {
    /// Conversation contents
    pub contents: Vec<GoogleContent>,
    /// System instruction
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<GoogleContent>,
    /// Generation configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GoogleGenerationConfig>,
    /// Tool definitions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<GoogleTool>>,
    /// Tool configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<GoogleToolConfig>,
}

/// Google content object containing role and parts
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleContent {
    /// Role ("user" or "model")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Content parts
    pub parts: Vec<GooglePart>,
}

/// Individual part within a Google content object
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GooglePart {
    /// Text content
    Text(String),
    /// Inline data (e.g. images)
    InlineData(GoogleInlineData),
    /// Function call from the model
    FunctionCall(GoogleFunctionCall),
    /// Function response from the user
    FunctionResponse(GoogleFunctionResponse),
}

/// Inline binary data (images, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleInlineData {
    /// MIME type (e.g. "image/png")
    pub mime_type: String,
    /// Base64-encoded data
    pub data: String,
}

/// Function call from the model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleFunctionCall {
    /// Function name
    pub name: String,
    /// Function arguments as JSON
    pub args: serde_json::Value,
}

/// Function response from the user
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleFunctionResponse {
    /// Function name
    pub name: String,
    /// Response content as JSON
    pub response: serde_json::Value,
}

/// Generation configuration parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleGenerationConfig {
    /// Sampling temperature
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Nucleus sampling threshold
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Top-k sampling
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Maximum output tokens
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Stop sequences
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    /// Candidate count (usually 1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_count: Option<u32>,
}

/// Google tool definition wrapper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleTool {
    /// Function declarations
    pub function_declarations: Vec<GoogleFunctionDeclaration>,
}

/// Google function declaration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleFunctionDeclaration {
    /// Function name
    pub name: String,
    /// Human-readable description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for parameters
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

/// Google tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleToolConfig {
    /// Function calling config
    pub function_calling_config: GoogleFunctionCallingConfig,
}

/// Function calling configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleFunctionCallingConfig {
    /// Mode: "AUTO", "ANY", "NONE"
    pub mode: String,
    /// Allowed function names (when mode is "ANY")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_function_names: Option<Vec<String>>,
}

// -- Response types --

/// Google `generateContent` response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleResponse {
    /// Generated candidates
    #[serde(default)]
    pub candidates: Vec<GoogleCandidate>,
    /// Token usage metadata
    #[serde(default)]
    pub usage_metadata: Option<GoogleUsageMetadata>,
}

/// Generated candidate
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleCandidate {
    /// Generated content
    pub content: GoogleContent,
    /// Finish reason
    #[serde(default)]
    pub finish_reason: Option<String>,
    /// Candidate index
    #[serde(default)]
    pub index: Option<u32>,
}

/// Token usage metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleUsageMetadata {
    /// Prompt token count
    #[serde(default)]
    pub prompt_token_count: u32,
    /// Candidates token count
    #[serde(default)]
    pub candidates_token_count: u32,
    /// Total token count
    #[serde(default)]
    pub total_token_count: u32,
}

// -- Streaming types --

/// Google streaming response wraps `generateContent` responses line by line
/// Each line is a complete `GoogleResponse` JSON object
pub type GoogleStreamChunk = GoogleResponse;

// -- Models list types --

/// Google models list response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleModelList {
    /// List of models
    #[serde(default)]
    pub models: Vec<GoogleModelInfo>,
    /// Pagination token
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

/// Google model info
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleModelInfo {
    /// Full model name (e.g. "models/gemini-pro")
    pub name: String,
    /// Display name
    #[serde(default)]
    pub display_name: Option<String>,
    /// Description
    #[serde(default)]
    pub description: Option<String>,
    /// Supported generation methods
    #[serde(default)]
    pub supported_generation_methods: Vec<String>,
}

// -- Error response --

/// Google error response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleErrorResponse {
    /// Error details
    pub error: GoogleErrorDetail,
}

/// Google error detail
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoogleErrorDetail {
    /// HTTP status code
    pub code: u32,
    /// Error message
    pub message: String,
    /// Error status string
    pub status: String,
}
