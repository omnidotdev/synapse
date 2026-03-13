//! Axum route handlers for OpenAI-compatible and Anthropic-compatible endpoints

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing};
use futures_util::{Stream, StreamExt};
use synapse_core::RequestContext;

use crate::convert;
use crate::error::LlmError;
use crate::protocol::anthropic::{AnthropicRequest, AnthropicResponse};
use crate::protocol::openai::{OpenAiModel, OpenAiModelList, OpenAiRequest, OpenAiResponse};
use crate::state::LlmState;
use crate::types::{CompletionRequest, StreamEvent};

/// Build the LLM router with all endpoints
pub fn llm_router(state: LlmState) -> Router {
    Router::new()
        // OpenAI-compatible endpoints
        .route("/v1/chat/completions", routing::post(openai_chat_completions))
        .route("/v1/models", routing::get(openai_list_models))
        // Anthropic-compatible endpoint
        .route("/v1/messages", routing::post(anthropic_messages))
        .with_state(state)
}

// -- OpenAI-compatible handlers --

/// Handle `POST /v1/chat/completions`
async fn openai_chat_completions(
    State(state): State<LlmState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    Json(wire_request): Json<OpenAiRequest>,
) -> Response {
    let is_stream = wire_request.stream.unwrap_or(false);
    let internal_request: CompletionRequest = wire_request.into();

    if is_stream {
        match state.complete_stream(internal_request, context).await {
            Ok((actual_model, stream)) => openai_stream_response(stream, actual_model).into_response(),
            Err(e) => error_to_openai_response(e),
        }
    } else {
        match state.complete(internal_request, context).await {
            Ok(response) => {
                let wire_response: OpenAiResponse = response.into();
                Json(wire_response).into_response()
            }
            Err(e) => error_to_openai_response(e),
        }
    }
}

/// Handle `GET /v1/models`
async fn openai_list_models(State(state): State<LlmState>) -> Response {
    let models = state.list_models().await;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let data: Vec<OpenAiModel> = models
        .into_iter()
        .map(|(display_name, _model_id)| OpenAiModel {
            id: display_name,
            object: "model".to_owned(),
            created: now,
            owned_by: "synapse".to_owned(),
        })
        .collect();

    let response = OpenAiModelList {
        object: "list".to_owned(),
        data,
    };

    Json(response).into_response()
}

/// Build a streaming SSE response in `OpenAI` format
fn openai_stream_response(
    stream: std::pin::Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>,
    model: String,
) -> Sse<impl Stream<Item = Result<Event, axum::Error>>> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let response_id = format!("chatcmpl-{now}");

    let event_stream = stream.map(move |result| match result {
        Ok(StreamEvent::Delta(delta)) => {
            let chunk = convert::openai::delta_to_openai_chunk(&delta, &response_id, &model, now);
            let data = serde_json::to_string(&chunk).unwrap_or_default();
            Ok(Event::default().data(data))
        }
        Ok(StreamEvent::Usage(usage)) => {
            let chunk = convert::openai::usage_to_openai_chunk(&usage, &response_id, &model, now);
            let data = serde_json::to_string(&chunk).unwrap_or_default();
            Ok(Event::default().data(data))
        }
        Ok(StreamEvent::Done) => Ok(Event::default().data("[DONE]")),
        Err(e) => {
            let error_data = serde_json::json!({
                "error": {
                    "message": e.to_string(),
                    "type": "streaming_error"
                }
            });
            Ok(Event::default().data(error_data.to_string()))
        }
    });

    Sse::new(event_stream).keep_alive(KeepAlive::default())
}

/// Convert an LLM error to an `OpenAI`-style JSON error response
#[allow(clippy::needless_pass_by_value)]
fn error_to_openai_response(error: LlmError) -> Response {
    use synapse_core::HttpError;

    let status = error.status_code();
    let body = serde_json::json!({
        "error": {
            "message": error.client_message(),
            "type": error.error_type(),
            "code": serde_json::Value::Null,
        }
    });

    (status, Json(body)).into_response()
}

// -- Anthropic-compatible handler --

/// Handle `POST /v1/messages`
async fn anthropic_messages(
    State(state): State<LlmState>,
    axum::Extension(context): axum::Extension<RequestContext>,
    Json(wire_request): Json<AnthropicRequest>,
) -> Response {
    let is_stream = wire_request.stream.unwrap_or(false);
    let internal_request: CompletionRequest = wire_request.into();

    if is_stream {
        match state.complete_stream(internal_request, context).await {
            Ok((actual_model, stream)) => anthropic_stream_response(stream, actual_model).into_response(),
            Err(e) => error_to_anthropic_response(e),
        }
    } else {
        match state.complete(internal_request, context).await {
            Ok(response) => {
                let wire_response: AnthropicResponse = response.into();
                Json(wire_response).into_response()
            }
            Err(e) => error_to_anthropic_response(e),
        }
    }
}

/// Build a streaming SSE response in Anthropic format
fn anthropic_stream_response(
    stream: std::pin::Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>,
    model: String,
) -> Sse<impl Stream<Item = Result<Event, axum::Error>>> {
    let response_id = format!("msg_{}", uuid_simple());

    let event_stream = stream.map(move |result| match result {
        Ok(event) => {
            let anthropic_events =
                convert::anthropic::internal_to_anthropic_stream_events(&event, &model, &response_id);

            // Return the first event as SSE
            anthropic_events.into_iter().next().map_or_else(
                || Ok(Event::default().comment("")),
                |anthropic_event| {
                    let event_type = anthropic_event_type(&anthropic_event);
                    let data = serde_json::to_string(&anthropic_event).unwrap_or_default();
                    Ok(Event::default().event(event_type).data(data))
                },
            )
        }
        Err(e) => {
            let error_data = serde_json::json!({
                "type": "error",
                "error": {
                    "type": "streaming_error",
                    "message": e.to_string()
                }
            });
            Ok(Event::default().event("error").data(error_data.to_string()))
        }
    });

    Sse::new(event_stream).keep_alive(KeepAlive::default())
}

/// Convert an LLM error to an Anthropic-style JSON error response
#[allow(clippy::needless_pass_by_value)]
fn error_to_anthropic_response(error: LlmError) -> Response {
    use synapse_core::HttpError;

    let status = error.status_code();
    let body = serde_json::json!({
        "type": "error",
        "error": {
            "type": error.error_type(),
            "message": error.client_message(),
        }
    });

    (status, Json(body)).into_response()
}

/// Get the SSE event type name for an Anthropic stream event
const fn anthropic_event_type(event: &crate::protocol::anthropic::AnthropicStreamEvent) -> &'static str {
    use crate::protocol::anthropic::AnthropicStreamEvent;

    match event {
        AnthropicStreamEvent::MessageStart { .. } => "message_start",
        AnthropicStreamEvent::ContentBlockStart { .. } => "content_block_start",
        AnthropicStreamEvent::ContentBlockDelta { .. } => "content_block_delta",
        AnthropicStreamEvent::ContentBlockStop { .. } => "content_block_stop",
        AnthropicStreamEvent::MessageDelta { .. } => "message_delta",
        AnthropicStreamEvent::MessageStop => "message_stop",
        AnthropicStreamEvent::Ping => "ping",
    }
}

/// Generate a simple unique ID
fn uuid_simple() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);

    format!("{now:x}{count:04x}")
}
