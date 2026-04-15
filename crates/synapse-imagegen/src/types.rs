use serde::{Deserialize, Serialize};

/// Image generation request following `OpenAI` API format
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ImageRequest {
    /// Text description of the desired image
    pub prompt: String,
    /// Model identifier (e.g. "dall-e-3", "gpt-image-1")
    pub model: String,
    /// Number of images to generate
    #[serde(default = "default_n")]
    pub n: u32,
    /// Size of generated images (e.g. "1024x1024")
    #[serde(default = "default_size")]
    pub size: String,
    /// Quality of generated images ("standard" or "hd")
    #[serde(default = "default_quality")]
    pub quality: String,
    /// Response format ("url" or "`b64_json`")
    #[serde(default = "default_response_format")]
    pub response_format: String,
    /// Unique identifier for the end user (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// Default number of images to generate
fn default_n() -> u32 {
    1
}

/// Default image size
fn default_size() -> String {
    "1024x1024".to_string()
}

/// Default image quality
fn default_quality() -> String {
    "standard".to_string()
}

/// Default response format
fn default_response_format() -> String {
    "url".to_string()
}

/// Single image entry in the response
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ImageData {
    /// URL of the generated image (when `response_format` is "url")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Base64-encoded image data (when `response_format` is "`b64_json`")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,
    /// Revised prompt used by the model (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revised_prompt: Option<String>,
}

/// Image generation response following `OpenAI` API format
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ImageResponse {
    /// Unix timestamp of when the response was created
    pub created: u64,
    /// Array of generated image results
    pub data: Vec<ImageData>,
}
