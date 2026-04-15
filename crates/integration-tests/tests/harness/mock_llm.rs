#![allow(dead_code)]
//! Mock LLM backend server for integration tests
//!
//! Implements a minimal OpenAI-compatible API that returns canned responses

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router, routing};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Mock LLM backend that returns predictable responses
pub struct MockLlm {
    addr: SocketAddr,
    shutdown: CancellationToken,
    state: Arc<MockLlmState>,
}

struct MockLlmState {
    request_count: AtomicU32,
    completion_count: AtomicU32,
    embedding_count: AtomicU32,
    imagegen_count: AtomicU32,
    /// Number of requests to fail before succeeding (0 = never fail)
    fail_count: AtomicU32,
    /// Custom response content (if set)
    response_content: Option<String>,
}

impl MockLlm {
    /// Start the mock server, returning immediately
    pub async fn start() -> anyhow::Result<Self> {
        Self::start_inner(0, None).await
    }

    /// Start a mock server that fails the first `n` requests with 500
    pub async fn start_failing(n: u32) -> anyhow::Result<Self> {
        Self::start_inner(n, None).await
    }

    /// Start a mock server with a custom response content
    pub async fn start_with_response(content: &str) -> anyhow::Result<Self> {
        Self::start_inner(0, Some(content.to_owned())).await
    }

    async fn start_inner(fail_count: u32, response_content: Option<String>) -> anyhow::Result<Self> {
        let state = Arc::new(MockLlmState {
            request_count: AtomicU32::new(0),
            completion_count: AtomicU32::new(0),
            embedding_count: AtomicU32::new(0),
            imagegen_count: AtomicU32::new(0),
            fail_count: AtomicU32::new(fail_count),
            response_content,
        });

        let app = Router::new()
            .route("/v1/chat/completions", routing::post(handle_chat_completions))
            .route("/v1/models", routing::get(handle_models))
            .route("/v1/embeddings", routing::post(handle_embeddings))
            .route("/v1/images/generations", routing::post(handle_imagegen))
            .with_state(Arc::clone(&state));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let shutdown = CancellationToken::new();
        let shutdown_clone = shutdown.clone();

        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    shutdown_clone.cancelled().await;
                })
                .await
                .ok();
        });

        Ok(Self { addr, shutdown, state })
    }

    /// Base URL for configuring the mock as a provider
    ///
    /// Includes `/v1` since the OpenAI provider appends paths like `/chat/completions`
    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    /// Number of completion requests received
    pub fn completion_count(&self) -> u32 {
        self.state.completion_count.load(Ordering::Relaxed)
    }

    /// Number of embedding requests received
    pub fn embedding_count(&self) -> u32 {
        self.state.embedding_count.load(Ordering::Relaxed)
    }

    /// Number of image generation requests received
    pub fn imagegen_count(&self) -> u32 {
        self.state.imagegen_count.load(Ordering::Relaxed)
    }
}

impl Drop for MockLlm {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

// -- Wire types matching OpenAI format --

#[derive(Debug, Deserialize)]
struct ChatCompletionRequest {
    model: String,
    #[allow(dead_code)]
    messages: Vec<ChatMessage>,
    #[serde(default)]
    stream: Option<bool>,
    #[serde(default)]
    tools: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    #[allow(dead_code)]
    role: String,
    #[allow(dead_code)]
    content: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct ChatCompletionResponse {
    id: String,
    object: String,
    created: u64,
    model: String,
    choices: Vec<Choice>,
    usage: Usage,
}

#[derive(Debug, Serialize)]
struct Choice {
    index: u32,
    message: ResponseMessage,
    finish_reason: String,
}

#[derive(Debug, Serialize)]
struct ResponseMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCallResponse>>,
}

#[derive(Debug, Serialize)]
struct ToolCallResponse {
    id: String,
    #[serde(rename = "type")]
    tool_type: String,
    function: FunctionCallResponse,
}

#[derive(Debug, Serialize)]
struct FunctionCallResponse {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[derive(Debug, Serialize)]
struct ModelListResponse {
    object: String,
    data: Vec<ModelObject>,
}

#[derive(Debug, Serialize)]
struct ModelObject {
    id: String,
    object: String,
    created: u64,
    owned_by: String,
}

// -- Stream chunk types --

#[derive(Debug, Serialize)]
struct StreamChunk {
    id: String,
    object: String,
    created: u64,
    model: String,
    choices: Vec<StreamChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<Usage>,
}

#[derive(Debug, Serialize)]
struct StreamChoice {
    index: u32,
    delta: StreamDelta,
    #[serde(skip_serializing_if = "Option::is_none")]
    finish_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct StreamDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Debug, Serialize)]
struct StreamToolCall {
    index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    tool_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function: Option<StreamFunctionCall>,
}

#[derive(Debug, Serialize)]
struct StreamFunctionCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<String>,
}

// -- Embeddings types --

#[derive(Debug, Deserialize)]
struct EmbeddingRequest {
    #[allow(dead_code)]
    input: serde_json::Value,
    model: String,
}

#[derive(Debug, Serialize)]
struct EmbeddingResponse {
    object: String,
    data: Vec<EmbeddingData>,
    model: String,
    usage: EmbeddingUsage,
}

#[derive(Debug, Serialize)]
struct EmbeddingData {
    object: String,
    embedding: Vec<f32>,
    index: usize,
}

#[derive(Debug, Serialize)]
struct EmbeddingUsage {
    prompt_tokens: u32,
    total_tokens: u32,
}

// -- Image generation types --

#[derive(Debug, Deserialize)]
struct ImageGenRequest {
    #[allow(dead_code)]
    prompt: String,
    #[allow(dead_code)]
    model: String,
}

#[derive(Debug, Serialize)]
struct ImageGenResponse {
    created: u64,
    data: Vec<ImageData>,
}

#[derive(Debug, Serialize)]
struct ImageData {
    url: String,
    revised_prompt: String,
}

// -- Handlers --

async fn handle_chat_completions(
    State(state): State<Arc<MockLlmState>>,
    Json(req): Json<ChatCompletionRequest>,
) -> impl IntoResponse {
    state.request_count.fetch_add(1, Ordering::Relaxed);
    state.completion_count.fetch_add(1, Ordering::Relaxed);

    // If fail_count > 0, decrement and return 500
    let remaining = state.fail_count.load(Ordering::Relaxed);
    if remaining > 0 {
        state.fail_count.fetch_sub(1, Ordering::Relaxed);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": {
                    "message": "mock server intentional failure",
                    "type": "server_error"
                }
            })),
        )
            .into_response();
    }

    // Check if streaming is requested
    if req.stream.unwrap_or(false) {
        return build_streaming_response(&state, &req).into_response();
    }

    let content = state.response_content.as_deref().unwrap_or("Hello from mock LLM");

    // If tools were provided, simulate a tool call
    let (content, tool_calls, finish_reason) = if req.tools.is_some() {
        (
            String::new(),
            Some(vec![ToolCallResponse {
                id: "call_test_123".to_owned(),
                tool_type: "function".to_owned(),
                function: FunctionCallResponse {
                    name: "get_weather".to_owned(),
                    arguments: r#"{"location":"San Francisco"}"#.to_owned(),
                },
            }]),
            "tool_calls".to_owned(),
        )
    } else {
        (content.to_owned(), None, "stop".to_owned())
    };

    let response = ChatCompletionResponse {
        id: "chatcmpl-test-123".to_owned(),
        object: "chat.completion".to_owned(),
        created: 1_700_000_000,
        model: req.model,
        choices: vec![Choice {
            index: 0,
            message: ResponseMessage {
                role: "assistant".to_owned(),
                content,
                tool_calls,
            },
            finish_reason,
        }],
        usage: Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        },
    };

    Json(response).into_response()
}

/// Build an SSE streaming response body
fn build_streaming_response(state: &MockLlmState, req: &ChatCompletionRequest) -> impl IntoResponse {
    let content = state
        .response_content
        .as_deref()
        .unwrap_or("Hello from mock LLM")
        .to_owned();
    let model = req.model.clone();
    let has_tools = req.tools.is_some();

    let id = "chatcmpl-test-stream";
    let created = 1_700_000_000u64;
    let mut body = String::new();

    if has_tools {
        // Tool call start chunk
        let chunk = StreamChunk {
            id: id.to_owned(),
            object: "chat.completion.chunk".to_owned(),
            created,
            model: model.clone(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: Some("assistant".to_owned()),
                    content: None,
                    tool_calls: Some(vec![StreamToolCall {
                        index: 0,
                        id: Some("call_test_stream".to_owned()),
                        tool_type: Some("function".to_owned()),
                        function: Some(StreamFunctionCall {
                            name: Some("get_weather".to_owned()),
                            arguments: None,
                        }),
                    }]),
                },
                finish_reason: None,
            }],
            usage: None,
        };
        body.push_str(&format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap()));

        // Tool call arguments chunk
        let chunk = StreamChunk {
            id: id.to_owned(),
            object: "chat.completion.chunk".to_owned(),
            created,
            model: model.clone(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: None,
                    content: None,
                    tool_calls: Some(vec![StreamToolCall {
                        index: 0,
                        id: None,
                        tool_type: None,
                        function: Some(StreamFunctionCall {
                            name: None,
                            arguments: Some(r#"{"location":"San Francisco"}"#.to_owned()),
                        }),
                    }]),
                },
                finish_reason: None,
            }],
            usage: None,
        };
        body.push_str(&format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap()));

        // Finish with tool_calls reason
        let chunk = StreamChunk {
            id: id.to_owned(),
            object: "chat.completion.chunk".to_owned(),
            created,
            model: model.clone(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: None,
                    content: None,
                    tool_calls: None,
                },
                finish_reason: Some("tool_calls".to_owned()),
            }],
            usage: None,
        };
        body.push_str(&format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap()));
    } else {
        // Role chunk
        let chunk = StreamChunk {
            id: id.to_owned(),
            object: "chat.completion.chunk".to_owned(),
            created,
            model: model.clone(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: Some("assistant".to_owned()),
                    content: Some(String::new()),
                    tool_calls: None,
                },
                finish_reason: None,
            }],
            usage: None,
        };
        body.push_str(&format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap()));

        // Content chunks (one per word)
        for word in content.split_whitespace() {
            let chunk = StreamChunk {
                id: id.to_owned(),
                object: "chat.completion.chunk".to_owned(),
                created,
                model: model.clone(),
                choices: vec![StreamChoice {
                    index: 0,
                    delta: StreamDelta {
                        role: None,
                        content: Some(format!("{word} ")),
                        tool_calls: None,
                    },
                    finish_reason: None,
                }],
                usage: None,
            };
            body.push_str(&format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap()));
        }

        // Finish reason chunk
        let chunk = StreamChunk {
            id: id.to_owned(),
            object: "chat.completion.chunk".to_owned(),
            created,
            model: model.clone(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: None,
                    content: None,
                    tool_calls: None,
                },
                finish_reason: Some("stop".to_owned()),
            }],
            usage: None,
        };
        body.push_str(&format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap()));
    }

    // Usage chunk
    let chunk = StreamChunk {
        id: id.to_owned(),
        object: "chat.completion.chunk".to_owned(),
        created,
        model,
        choices: vec![],
        usage: Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        }),
    };
    body.push_str(&format!("data: {}\n\n", serde_json::to_string(&chunk).unwrap()));

    // Done marker
    body.push_str("data: [DONE]\n\n");

    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
}

async fn handle_models(State(state): State<Arc<MockLlmState>>) -> impl IntoResponse {
    state.request_count.fetch_add(1, Ordering::Relaxed);

    let response = ModelListResponse {
        object: "list".to_owned(),
        data: vec![ModelObject {
            id: "mock-model-1".to_owned(),
            object: "model".to_owned(),
            created: 1_700_000_000,
            owned_by: "mock".to_owned(),
        }],
    };

    Json(response)
}

async fn handle_embeddings(
    State(state): State<Arc<MockLlmState>>,
    Json(req): Json<EmbeddingRequest>,
) -> impl IntoResponse {
    state.request_count.fetch_add(1, Ordering::Relaxed);
    state.embedding_count.fetch_add(1, Ordering::Relaxed);

    let response = EmbeddingResponse {
        object: "list".to_owned(),
        data: vec![EmbeddingData {
            object: "embedding".to_owned(),
            embedding: vec![0.1, 0.2, 0.3, 0.4, 0.5],
            index: 0,
        }],
        model: req.model,
        usage: EmbeddingUsage {
            prompt_tokens: 8,
            total_tokens: 8,
        },
    };

    Json(response)
}

async fn handle_imagegen(
    State(state): State<Arc<MockLlmState>>,
    Json(_req): Json<ImageGenRequest>,
) -> impl IntoResponse {
    state.request_count.fetch_add(1, Ordering::Relaxed);
    state.imagegen_count.fetch_add(1, Ordering::Relaxed);

    let response = ImageGenResponse {
        created: 1_700_000_000,
        data: vec![ImageData {
            url: "https://example.com/mock-image.png".to_owned(),
            revised_prompt: "A mock image for testing".to_owned(),
        }],
    };

    Json(response)
}
