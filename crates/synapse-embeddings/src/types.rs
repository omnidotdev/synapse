use serde::{Deserialize, Serialize};

/// Embedding input that accepts either a single string or array of strings
///
/// Matches `OpenAI` API behavior where `input` can be a string or array
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum EmbedInput {
    /// Single text input
    Single(String),
    /// Multiple text inputs
    Multiple(Vec<String>),
}

impl EmbedInput {
    /// Return the inputs as a slice of strings
    #[must_use]
    pub fn as_vec(&self) -> Vec<&str> {
        match self {
            Self::Single(s) => vec![s.as_str()],
            Self::Multiple(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

/// Embedding request following `OpenAI` API format
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EmbeddingRequest {
    /// Input text(s) to embed
    pub input: EmbedInput,
    /// Model identifier (e.g. "text-embedding-3-small")
    pub model: String,
    /// Encoding format for the embeddings ("float" or "base64")
    #[serde(default = "default_encoding_format")]
    pub encoding_format: String,
    /// Number of dimensions for the output embeddings (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<u32>,
    /// Unique identifier for the end user (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// Default encoding format per `OpenAI` API spec
fn default_encoding_format() -> String {
    "float".to_string()
}

/// Single embedding entry in the response
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EmbeddingData {
    /// The embedding object type (always "embedding")
    pub object: String,
    /// The embedding vector
    pub embedding: Vec<f32>,
    /// Index of this embedding in the request input array
    pub index: usize,
}

/// Token usage information for an embedding request
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EmbeddingUsage {
    /// Number of tokens in the input
    pub prompt_tokens: u32,
    /// Total tokens used (same as `prompt_tokens` for embeddings)
    pub total_tokens: u32,
}

/// Embedding response following `OpenAI` API format
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EmbeddingResponse {
    /// Object type (always "list")
    pub object: String,
    /// Array of embedding results
    pub data: Vec<EmbeddingData>,
    /// Model used to generate the embeddings
    pub model: String,
    /// Token usage information
    pub usage: EmbeddingUsage,
}
