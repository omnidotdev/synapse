use axum::body::Body;
use secrecy::SecretString;
use synapse_core::{Authentication, ClientIdentity};

use crate::types::TranscriptionRequest;

/// Header name for user-provided API keys (BYOK)
const PROVIDER_API_KEY_HEADER: &str = "X-Provider-API-Key";

/// Runtime context for STT provider requests
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RequestContext {
    pub parts: http::request::Parts,

    /// User-provided API key that overrides the configured key
    pub api_key: Option<SecretString>,

    /// Client identity information for rate limiting and access control
    pub client_identity: Option<ClientIdentity>,

    pub authentication: Authentication,
}

#[allow(dead_code)]
impl RequestContext {
    pub fn headers(&self) -> &axum::http::HeaderMap {
        &self.parts.headers
    }
}

/// Extractor for multipart form data containing audio files
pub struct ExtractMultipart(pub RequestContext, pub TranscriptionRequest);

/// Body limit for audio uploads (32 MiB)
const BODY_LIMIT_BYTES: usize = 32 << 20;

impl<S> axum::extract::FromRequest<S> for ExtractMultipart
where
    S: Send + Sync,
{
    type Rejection = axum::response::Response;

    #[allow(clippy::too_many_lines)]
    async fn from_request(request: http::Request<Body>, _state: &S) -> Result<Self, Self::Rejection> {
        use axum::response::IntoResponse;

        let (mut parts, body) = request.into_parts();

        // Verify content type is multipart/form-data
        let content_type = parts
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !content_type.starts_with("multipart/form-data") {
            return Err((
                axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Unsupported Content-Type, expected: 'Content-Type: multipart/form-data'",
            )
                .into_response());
        }

        let bytes = axum::body::to_bytes(body, BODY_LIMIT_BYTES).await.map_err(|err| {
            (
                axum::http::StatusCode::BAD_REQUEST,
                format!("Failed to read request body: {err}"),
            )
                .into_response()
        })?;

        // Reassemble the request for multipart parsing
        let mut rebuilt = http::Request::builder()
            .method(parts.method.clone())
            .uri(parts.uri.clone());

        for (key, value) in &parts.headers {
            rebuilt = rebuilt.header(key, value);
        }

        let rebuilt = rebuilt.body(Body::from(bytes)).map_err(|e| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to rebuild request: {e}"),
            )
                .into_response()
        })?;

        let mut multipart = axum::extract::Multipart::from_request(rebuilt, &())
            .await
            .map_err(|e| {
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    format!("Failed to parse multipart form: {e}"),
                )
                    .into_response()
            })?;

        let mut audio: Option<Vec<u8>> = None;
        let mut filename = String::from("audio.wav");
        let mut file_content_type = String::from("audio/wav");
        let mut model = String::new();
        let mut language: Option<String> = None;
        let mut prompt: Option<String> = None;
        let mut response_format: Option<String> = None;
        let mut temperature: Option<f32> = None;

        while let Ok(Some(field)) = multipart.next_field().await {
            let field_name = field.name().unwrap_or("").to_string();

            match field_name.as_str() {
                "file" => {
                    if let Some(name) = field.file_name() {
                        filename = name.to_string();
                    }
                    if let Some(ct) = field.content_type() {
                        file_content_type = ct.to_string();
                    }
                    audio = Some(
                        field
                            .bytes()
                            .await
                            .map_err(|e| {
                                (
                                    axum::http::StatusCode::BAD_REQUEST,
                                    format!("Failed to read audio data: {e}"),
                                )
                                    .into_response()
                            })?
                            .to_vec(),
                    );
                }
                "model" => {
                    model = field.text().await.map_err(|e| {
                        (
                            axum::http::StatusCode::BAD_REQUEST,
                            format!("Failed to read model field: {e}"),
                        )
                            .into_response()
                    })?;
                }
                "language" => {
                    language = Some(field.text().await.map_err(|e| {
                        (
                            axum::http::StatusCode::BAD_REQUEST,
                            format!("Failed to read language field: {e}"),
                        )
                            .into_response()
                    })?);
                }
                "prompt" => {
                    prompt = Some(field.text().await.map_err(|e| {
                        (
                            axum::http::StatusCode::BAD_REQUEST,
                            format!("Failed to read prompt field: {e}"),
                        )
                            .into_response()
                    })?);
                }
                "response_format" => {
                    response_format = Some(field.text().await.map_err(|e| {
                        (
                            axum::http::StatusCode::BAD_REQUEST,
                            format!("Failed to read response_format field: {e}"),
                        )
                            .into_response()
                    })?);
                }
                "temperature" => {
                    let temp_str = field.text().await.map_err(|e| {
                        (
                            axum::http::StatusCode::BAD_REQUEST,
                            format!("Failed to read temperature field: {e}"),
                        )
                            .into_response()
                    })?;
                    temperature = Some(temp_str.parse::<f32>().map_err(|e| {
                        (
                            axum::http::StatusCode::BAD_REQUEST,
                            format!("Invalid temperature value: {e}"),
                        )
                            .into_response()
                    })?);
                }
                _ => {
                    // Skip unknown fields
                }
            }
        }

        let audio = audio.ok_or_else(|| {
            (
                axum::http::StatusCode::BAD_REQUEST,
                "Missing required 'file' field in multipart form",
            )
                .into_response()
        })?;

        if model.is_empty() {
            return Err((
                axum::http::StatusCode::BAD_REQUEST,
                "Missing required 'model' field in multipart form",
            )
                .into_response());
        }

        let transcription_request = TranscriptionRequest {
            audio,
            filename,
            content_type: file_content_type,
            model,
            language,
            prompt,
            response_format,
            temperature,
        };

        let ctx = RequestContext {
            api_key: parts
                .headers
                .get(PROVIDER_API_KEY_HEADER)
                .and_then(|value| value.to_str().map(str::to_string).ok())
                .map(SecretString::from),
            client_identity: parts.extensions.remove(),
            authentication: parts.extensions.remove().unwrap_or_default(),
            parts,
        };

        Ok(Self(ctx, transcription_request))
    }
}
