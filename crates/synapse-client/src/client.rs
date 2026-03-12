use std::fmt;
use std::pin::Pin;
#[cfg(feature = "embedded")]
use std::sync::Arc;
#[cfg(feature = "embedded")]
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures::stream::{self, Stream, StreamExt};
use reqwest::header::AUTHORIZATION;
use serde::Deserialize;
use url::Url;

use crate::error::{Result, SynapseClientError};
use crate::types::{
    ChatEvent, ChatRequest, ChatResponse, EmbedRequest, EmbeddingResponse, ImageRequest, ImageResponse, McpTool, Model,
    ModelList, SpeechRequest, StreamChunk, ToolResult, ToolSearchResult, Transcription,
};

/// Backend mode for the Synapse client
enum Backend {
    /// HTTP client talking to a remote Synapse server
    Remote {
        base_url: Url,
        http: reqwest::Client,
        api_key: Option<String>,
    },
    /// In-process synapse-llm (requires `embedded` feature)
    #[cfg(feature = "embedded")]
    Embedded {
        state: synapse_llm::LlmState,
        stt_server: Option<Arc<stt::Server>>,
        tts_server: Option<Arc<tts::Server>>,
    },
}

impl Clone for Backend {
    fn clone(&self) -> Self {
        match self {
            Self::Remote {
                base_url,
                http,
                api_key,
            } => Self::Remote {
                base_url: base_url.clone(),
                http: http.clone(),
                api_key: api_key.clone(),
            },
            #[cfg(feature = "embedded")]
            Self::Embedded {
                state,
                stt_server,
                tts_server,
            } => Self::Embedded {
                state: state.clone(),
                stt_server: stt_server.clone(),
                tts_server: tts_server.clone(),
            },
        }
    }
}

impl fmt::Debug for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Remote { base_url, .. } => f
                .debug_struct("Remote")
                .field("base_url", base_url)
                .finish_non_exhaustive(),
            #[cfg(feature = "embedded")]
            Self::Embedded { .. } => f.debug_struct("Embedded").finish_non_exhaustive(),
        }
    }
}

/// Typed client for the Synapse AI router
#[derive(Debug, Clone)]
pub struct SynapseClient {
    backend: Backend,
}

impl SynapseClient {
    /// Create a new client pointing at the given base URL
    ///
    /// # Errors
    ///
    /// Returns an error if the URL is invalid
    pub fn new(base_url: &str) -> Result<Self> {
        let base_url =
            Url::parse(base_url).map_err(|e| SynapseClientError::Config(format!("invalid base URL: {e}")))?;

        Ok(Self {
            backend: Backend::Remote {
                base_url,
                http: reqwest::Client::new(),
                api_key: None,
            },
        })
    }

    /// Set the API key for authentication (remote mode only)
    #[must_use]
    pub fn with_api_key(mut self, api_key: String) -> Self {
        match &mut self.backend {
            Backend::Remote { api_key: key, .. } => *key = Some(api_key),
            #[cfg(feature = "embedded")]
            Backend::Embedded { .. } => {}
        }

        self
    }

    /// Get the base URL (remote mode only)
    ///
    /// # Panics
    ///
    /// Panics if the fallback URL fails to parse (should never happen)
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn base_url(&self) -> &Url {
        match &self.backend {
            Backend::Remote { base_url, .. } => base_url,
            #[cfg(feature = "embedded")]
            Backend::Embedded { .. } => {
                // Embedded mode has no remote URL
                static FALLBACK: std::sync::OnceLock<Url> = std::sync::OnceLock::new();
                FALLBACK.get_or_init(|| Url::parse("http://localhost").expect("valid fallback URL"))
            }
        }
    }

    // -- LLM --

    /// Send a chat completion request (non-streaming)
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be parsed
    pub async fn chat_completion(&self, req: &ChatRequest) -> Result<ChatResponse> {
        match &self.backend {
            Backend::Remote {
                base_url,
                http,
                api_key,
            } => {
                let url = make_url(base_url, "/v1/chat/completions");

                let request = ChatRequest {
                    stream: false,
                    ..req.clone()
                };

                let response = make_request(http, reqwest::Method::POST, &url, api_key.as_deref())
                    .json(&request)
                    .send()
                    .await?;

                handle_error(response).await?.json().await.map_err(Into::into)
            }
            #[cfg(feature = "embedded")]
            Backend::Embedded { state, .. } => {
                let internal_req = crate::embedded::to_completion_request(req);
                let context = synapse_core::RequestContext::empty();
                let response = state
                    .complete(internal_req, context)
                    .await
                    .map_err(|e| crate::embedded::from_llm_error(&e))?;
                Ok(crate::embedded::from_completion_response(response))
            }
        }
    }

    /// Send a streaming chat completion request
    ///
    /// Returns a stream of `ChatEvent`s parsed from SSE
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails
    pub async fn chat_completion_stream(
        &self,
        req: &ChatRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatEvent>> + Send>>> {
        match &self.backend {
            Backend::Remote {
                base_url,
                http,
                api_key,
            } => {
                let url = make_url(base_url, "/v1/chat/completions");

                let mut request = req.clone();
                request.stream = true;

                let response = make_request(http, reqwest::Method::POST, &url, api_key.as_deref())
                    .json(&request)
                    .send()
                    .await?;

                let response = handle_error(response).await?;
                let byte_stream = response.bytes_stream();

                let event_stream = parse_sse_stream(byte_stream);

                Ok(Box::pin(event_stream))
            }
            #[cfg(feature = "embedded")]
            Backend::Embedded { state, .. } => {
                let internal_req = crate::embedded::to_completion_request(req);
                let context = synapse_core::RequestContext::empty();
                let (_, stream) = state
                    .complete_stream(internal_req, context)
                    .await
                    .map_err(|e| crate::embedded::from_llm_error(&e))?;
                Ok(crate::embedded::stream_to_chat_events(stream))
            }
        }
    }

    /// List available models
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails
    pub async fn list_models(&self) -> Result<Vec<Model>> {
        match &self.backend {
            Backend::Remote {
                base_url,
                http,
                api_key,
            } => {
                let url = make_url(base_url, "/v1/models");

                let response = make_request(http, reqwest::Method::GET, &url, api_key.as_deref())
                    .send()
                    .await?;

                let list: ModelList = handle_error(response).await?.json().await?;
                Ok(list.data)
            }
            #[cfg(feature = "embedded")]
            Backend::Embedded { state, .. } => {
                let models = state.list_models().await;
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                Ok(models
                    .into_iter()
                    .map(|(id, _)| Model {
                        id,
                        object: "model".to_owned(),
                        created: now,
                        owned_by: "synapse".to_owned(),
                    })
                    .collect())
            }
        }
    }

    // -- STT --

    /// Transcribe audio
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails
    pub async fn transcribe(&self, audio: Bytes, filename: &str, model: &str) -> Result<Transcription> {
        match &self.backend {
            Backend::Remote {
                base_url,
                http,
                api_key,
            } => {
                let url = make_url(base_url, "/v1/audio/transcriptions");

                let part = reqwest::multipart::Part::bytes(audio.to_vec())
                    .file_name(filename.to_owned())
                    .mime_str("audio/wav")
                    .map_err(|e| SynapseClientError::Config(format!("invalid mime type: {e}")))?;

                let form = reqwest::multipart::Form::new()
                    .text("model", model.to_owned())
                    .part("file", part);

                let response = make_request(http, reqwest::Method::POST, &url, api_key.as_deref())
                    .multipart(form)
                    .send()
                    .await?;

                handle_error(response).await?.json().await.map_err(Into::into)
            }
            #[cfg(feature = "embedded")]
            Backend::Embedded { stt_server, .. } => {
                let server = stt_server
                    .as_ref()
                    .ok_or(SynapseClientError::Config("no STT provider configured".to_owned()))?;

                let req = stt::TranscriptionRequest {
                    audio: audio.to_vec(),
                    filename: filename.to_owned(),
                    content_type: "audio/wav".to_owned(),
                    model: model.to_owned(),
                    language: None,
                    prompt: None,
                    response_format: None,
                    temperature: None,
                };

                let (parts, ()) = http::Request::new(()).into_parts();
                let ctx = stt::RequestContext {
                    parts,
                    api_key: None,
                    client_identity: None,
                    authentication: synapse_core::Authentication::default(),
                };

                let resp = server
                    .transcribe(req, &ctx)
                    .await
                    .map_err(|e| SynapseClientError::Config(format!("STT: {e}")))?;

                Ok(Transcription { text: resp.text })
            }
        }
    }

    // -- TTS --

    /// Synthesize text to speech
    ///
    /// Returns raw audio bytes
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails
    pub async fn synthesize(&self, req: &SpeechRequest) -> Result<Bytes> {
        match &self.backend {
            Backend::Remote {
                base_url,
                http,
                api_key,
            } => {
                let url = make_url(base_url, "/v1/audio/speech");

                let response = make_request(http, reqwest::Method::POST, &url, api_key.as_deref())
                    .json(req)
                    .send()
                    .await?;

                Ok(handle_error(response).await?.bytes().await?)
            }
            #[cfg(feature = "embedded")]
            Backend::Embedded { tts_server, .. } => {
                let server = tts_server
                    .as_ref()
                    .ok_or(SynapseClientError::Config("no TTS provider configured".to_owned()))?;

                let tts_req = tts::SpeechRequest {
                    model: req.model.clone(),
                    input: req.input.clone(),
                    voice: req.voice.clone(),
                    response_format: req.response_format.clone(),
                    speed: req.speed,
                };

                let (parts, ()) = http::Request::new(()).into_parts();
                let ctx = tts::RequestContext {
                    parts,
                    api_key: None,
                    client_identity: None,
                    authentication: synapse_core::Authentication::default(),
                };

                let resp = server
                    .synthesize(tts_req, &ctx)
                    .await
                    .map_err(|e| SynapseClientError::Config(format!("TTS: {e}")))?;

                Ok(Bytes::from(resp.audio))
            }
        }
    }

    // -- MCP --

    /// List available MCP tools
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails
    pub async fn list_tools(&self, server: Option<&str>) -> Result<Vec<McpTool>> {
        match &self.backend {
            Backend::Remote {
                base_url,
                http,
                api_key,
            } => {
                #[derive(Deserialize)]
                struct Resp {
                    tools: Vec<McpTool>,
                }

                let url = make_url(base_url, "/mcp/tools/list");

                let body = serde_json::json!({
                    "server": server,
                });

                let response = make_request(http, reqwest::Method::POST, &url, api_key.as_deref())
                    .json(&body)
                    .send()
                    .await?;

                let resp: Resp = handle_error(response).await?.json().await?;
                Ok(resp.tools)
            }
            #[cfg(feature = "embedded")]
            Backend::Embedded { .. } => Err(SynapseClientError::Config(
                "feature not available in embedded mode".to_owned(),
            )),
        }
    }

    /// Call an MCP tool
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails
    pub async fn call_tool(&self, name: &str, arguments: serde_json::Value) -> Result<ToolResult> {
        match &self.backend {
            Backend::Remote {
                base_url,
                http,
                api_key,
            } => {
                let url = make_url(base_url, "/mcp/tools/call");

                let body = serde_json::json!({
                    "name": name,
                    "arguments": arguments,
                });

                let response = make_request(http, reqwest::Method::POST, &url, api_key.as_deref())
                    .json(&body)
                    .send()
                    .await?;

                handle_error(response).await?.json().await.map_err(Into::into)
            }
            #[cfg(feature = "embedded")]
            Backend::Embedded { .. } => Err(SynapseClientError::Config(
                "feature not available in embedded mode".to_owned(),
            )),
        }
    }

    /// Search MCP tools by query
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails
    pub async fn search_tools(&self, query: &str, limit: Option<usize>) -> Result<Vec<ToolSearchResult>> {
        match &self.backend {
            Backend::Remote {
                base_url,
                http,
                api_key,
            } => {
                #[derive(Deserialize)]
                struct Resp {
                    results: Vec<ToolSearchResult>,
                }

                let mut url = make_url(base_url, "/mcp/search");
                url.query_pairs_mut().append_pair("q", query);
                if let Some(limit) = limit {
                    url.query_pairs_mut().append_pair("limit", &limit.to_string());
                }

                let response = make_request(http, reqwest::Method::GET, &url, api_key.as_deref())
                    .send()
                    .await?;

                let resp: Resp = handle_error(response).await?.json().await?;
                Ok(resp.results)
            }
            #[cfg(feature = "embedded")]
            Backend::Embedded { .. } => Err(SynapseClientError::Config(
                "feature not available in embedded mode".to_owned(),
            )),
        }
    }

    // -- Embeddings --

    /// Generate embeddings for text input
    pub async fn embed(&self, req: &EmbedRequest) -> Result<EmbeddingResponse> {
        match &self.backend {
            Backend::Remote {
                base_url,
                http,
                api_key,
            } => {
                let url = make_url(base_url, "/v1/embeddings");

                let response = make_request(http, reqwest::Method::POST, &url, api_key.as_deref())
                    .json(req)
                    .send()
                    .await?;

                handle_error(response).await?.json().await.map_err(Into::into)
            }
            #[cfg(feature = "embedded")]
            Backend::Embedded { .. } => Err(SynapseClientError::Config(
                "feature not available in embedded mode".to_owned(),
            )),
        }
    }

    // -- Image Generation --

    /// Generate images from a text prompt
    pub async fn generate_image(&self, req: &ImageRequest) -> Result<ImageResponse> {
        match &self.backend {
            Backend::Remote {
                base_url,
                http,
                api_key,
            } => {
                let url = make_url(base_url, "/v1/images/generations");

                let response = make_request(http, reqwest::Method::POST, &url, api_key.as_deref())
                    .json(req)
                    .send()
                    .await?;

                handle_error(response).await?.json().await.map_err(Into::into)
            }
            #[cfg(feature = "embedded")]
            Backend::Embedded { .. } => Err(SynapseClientError::Config(
                "feature not available in embedded mode".to_owned(),
            )),
        }
    }
}

#[cfg(feature = "embedded")]
impl SynapseClient {
    /// Create a client using synapse-llm in-process (no HTTP server needed)
    ///
    /// STT/TTS initialization failures are soft — the client will still work
    /// for LLM chat, but `transcribe()`/`synthesize()` will return errors.
    ///
    /// # Errors
    ///
    /// Returns an error if the LLM providers fail to initialize
    pub async fn embedded(config: synapse_config::Config) -> Result<Self> {
        // Build STT/TTS servers before moving config.llm
        let stt_server = stt::SttServerBuilder::new(&config).build().ok().map(Arc::new);

        let tts_server = tts::TtsServerBuilder::new(&config).build().ok().map(Arc::new);

        let state = synapse_llm::LlmState::from_config(config.llm)
            .await
            .map_err(|e| SynapseClientError::Config(format!("failed to initialize LLM state: {e}")))?;

        Ok(Self {
            backend: Backend::Embedded {
                state,
                stt_server,
                tts_server,
            },
        })
    }
}

// -- Helper functions --

/// Build a URL from a base and path
fn make_url(base_url: &Url, path: &str) -> Url {
    let mut url = base_url.clone();
    url.set_path(path);
    url
}

/// Build an authenticated request
fn make_request(
    http: &reqwest::Client,
    method: reqwest::Method,
    url: &Url,
    api_key: Option<&str>,
) -> reqwest::RequestBuilder {
    let mut builder = http.request(method, url.as_str());

    if let Some(key) = api_key {
        builder = builder.header(AUTHORIZATION, format!("Bearer {key}"));
    }

    builder
}

/// Check an HTTP response for errors
async fn handle_error(response: reqwest::Response) -> Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    // Try to parse error body
    let body = response.text().await.unwrap_or_default();
    let (error_type, message) = parse_error_body(&body);

    Err(SynapseClientError::Api {
        status: status.as_u16(),
        error_type,
        message,
    })
}

/// Parse an error response body into (type, message)
fn parse_error_body(body: &str) -> (String, String) {
    serde_json::from_str::<serde_json::Value>(body).map_or_else(
        |_| ("unknown".to_owned(), body.to_owned()),
        |json| {
            let error = &json["error"];
            let error_type = error["type"].as_str().unwrap_or("unknown").to_owned();
            let message = error["message"].as_str().unwrap_or(body).to_owned();
            (error_type, message)
        },
    )
}

/// Parse a byte stream of SSE data into `ChatEvent`s
fn parse_sse_stream<S>(byte_stream: S) -> impl Stream<Item = Result<ChatEvent>>
where
    S: Stream<Item = std::result::Result<Bytes, reqwest::Error>> + Send + 'static,
{
    // Buffer for incomplete lines across chunks
    let line_stream = byte_stream
        .map(|result| result.map_err(SynapseClientError::Http))
        .scan(String::new(), |buffer, result| {
            let bytes = match result {
                Ok(b) => b,
                Err(e) => return std::future::ready(Some(vec![Err(e)])),
            };

            let text = String::from_utf8_lossy(&bytes);
            buffer.push_str(&text);

            let mut events = Vec::new();
            while let Some(pos) = buffer.find("\n\n") {
                let chunk = buffer[..pos].to_owned();
                *buffer = buffer[pos + 2..].to_owned();

                for line in chunk.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        events.push(Ok(data.to_owned()));
                    }
                }
            }

            std::future::ready(Some(events))
        })
        .flat_map(stream::iter);

    // Parse each SSE data line into ChatEvents.
    // Usage may arrive in a separate chunk before the finish chunk, so
    // accumulate it and attach to the final Done event.
    line_stream
        .scan(None::<crate::types::Usage>, |accumulated_usage, result| {
            let events = match result {
                Err(e) => vec![Err(e)],
                Ok(data) => {
                    if data == "[DONE]" {
                        return std::future::ready(Some(vec![Ok(ChatEvent::Done {
                            finish_reason: None,
                            usage: accumulated_usage.take(),
                        })]));
                    }

                    match serde_json::from_str::<StreamChunk>(&data) {
                        Ok(chunk) => {
                            // Store usage from any chunk (including usage-only chunks)
                            if let Some(ref usage) = chunk.usage {
                                *accumulated_usage = Some(usage.clone());
                            }
                            chunk_to_events(chunk, accumulated_usage)
                        }
                        Err(e) => vec![Err(SynapseClientError::Parse(format!(
                            "failed to parse stream chunk: {e}"
                        )))],
                    }
                }
            };
            std::future::ready(Some(events))
        })
        .flat_map(stream::iter)
}

/// Convert a parsed stream chunk into `ChatEvent`s
///
/// A single chunk can produce multiple events (e.g. content + finish in the
/// same SSE frame, which models like Kimi K2.5 do via thinking mode).
///
/// `accumulated_usage` is consumed when a `Done` event is emitted, attaching
/// the usage data that may have arrived in an earlier chunk.
fn chunk_to_events(chunk: StreamChunk, accumulated_usage: &mut Option<crate::types::Usage>) -> Vec<Result<ChatEvent>> {
    let Some(choice) = chunk.choices.into_iter().next() else {
        // Usage-only chunks have no choices — usage is already stored by the
        // caller, so there is nothing to emit here.
        return vec![];
    };

    let mut events = Vec::new();

    // Emit content delta first so it is not lost when finish_reason is
    // present in the same chunk (Kimi K2.5 thinking mode sends the entire
    // answer alongside `finish_reason: "stop"` in the final frame)
    if let Some(content) = choice.delta.content {
        events.push(Ok(ChatEvent::ContentDelta(content)));
    }

    // Tool calls
    if let Some(tool_calls) = choice.delta.tool_calls {
        for tc in tool_calls {
            if let Some(ref func) = tc.function {
                if let (Some(id), Some(name)) = (&tc.id, &func.name) {
                    events.push(Ok(ChatEvent::ToolCallStart {
                        index: tc.index,
                        id: id.clone(),
                        name: name.clone(),
                    }));
                }
                if let Some(ref args) = func.arguments {
                    events.push(Ok(ChatEvent::ToolCallDelta {
                        index: tc.index,
                        arguments: args.clone(),
                    }));
                }
            }
        }
    }

    // Finish (must come after content/tool events).
    // Prefer usage from this chunk, fall back to accumulated usage from
    // an earlier usage-only chunk.
    if let Some(reason) = choice.finish_reason {
        let usage = chunk.usage.or_else(|| accumulated_usage.take());
        events.push(Ok(ChatEvent::Done {
            finish_reason: Some(reason),
            usage,
        }));
    }

    events
}
