//! Type conversion and embedded backend logic
//!
//! Convert between synapse-client's OpenAI-compatible wire types
//! and synapse-llm's internal canonical types

use std::pin::Pin;

use futures::stream::{Stream, StreamExt};
use synapse_llm::types as llm;

use crate::error::{Result, SynapseClientError};
use crate::types;

/// Convert a client `ChatRequest` to an internal `CompletionRequest`
pub fn to_completion_request(req: &types::ChatRequest) -> llm::CompletionRequest {
    let messages = req.messages.iter().map(to_internal_message).collect();

    let tools = req.tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|t| llm::ToolDefinition {
                tool_type: t.tool_type.clone(),
                function: llm::FunctionDefinition {
                    name: t.function.name.clone(),
                    description: t.function.description.clone(),
                    parameters: t.function.parameters.clone(),
                },
            })
            .collect()
    });

    let tool_choice = req
        .tool_choice
        .as_ref()
        .and_then(|v| serde_json::from_value::<llm::ToolChoice>(v.clone()).ok());

    llm::CompletionRequest {
        model: req.model.clone(),
        messages,
        params: llm::CompletionParams {
            temperature: req.temperature,
            top_p: req.top_p,
            max_tokens: req.max_tokens,
            stop: req.stop.clone(),
            ..Default::default()
        },
        tools,
        tool_choice,
        stream: req.stream,
    }
}

/// Convert a client `Message` to an internal `Message`
fn to_internal_message(msg: &types::Message) -> llm::Message {
    let role = match msg.role.as_str() {
        "system" => llm::Role::System,
        "assistant" => llm::Role::Assistant,
        "tool" => llm::Role::Tool,
        _ => llm::Role::User,
    };

    let content = match &msg.content {
        serde_json::Value::String(s) => llm::Content::Text(s.clone()),
        other => llm::Content::Text(other.to_string()),
    };

    let tool_calls = msg.tool_calls.as_ref().map(|tcs| {
        tcs.iter()
            .map(|tc| llm::ToolCall {
                id: tc.id.clone(),
                function: llm::FunctionCall {
                    name: tc.function.name.clone(),
                    arguments: tc.function.arguments.clone(),
                },
            })
            .collect()
    });

    llm::Message {
        role,
        content,
        name: None,
        tool_calls,
        tool_call_id: msg.tool_call_id.clone(),
    }
}

/// Convert an internal `CompletionResponse` to a client `ChatResponse`
pub fn from_completion_response(resp: llm::CompletionResponse) -> types::ChatResponse {
    let choices = resp
        .choices
        .into_iter()
        .map(|c| types::Choice {
            index: c.index,
            message: types::ChoiceMessage {
                role: c.message.role,
                content: c.message.content,
                tool_calls: c.message.tool_calls.map(|tcs| {
                    tcs.into_iter()
                        .map(|tc| types::ToolCall {
                            id: tc.id,
                            tool_type: "function".to_owned(),
                            function: types::FunctionCall {
                                name: tc.function.name,
                                arguments: tc.function.arguments,
                            },
                        })
                        .collect()
                }),
            },
            finish_reason: c.finish_reason.map(|r| {
                serde_json::to_value(&r)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_else(|| format!("{r:?}").to_lowercase())
            }),
        })
        .collect();

    let usage = resp.usage.map(|u| types::Usage {
        prompt_tokens: u.prompt_tokens,
        completion_tokens: u.completion_tokens,
        total_tokens: u.total_tokens,
    });

    types::ChatResponse {
        id: resp.id,
        object: resp.object,
        created: resp.created,
        model: resp.model,
        choices,
        usage,
    }
}

/// Convert an internal `StreamEvent` stream to a client `ChatEvent` stream
pub fn stream_to_chat_events(
    stream: Pin<Box<dyn Stream<Item = std::result::Result<llm::StreamEvent, synapse_llm::LlmError>> + Send>>,
) -> Pin<Box<dyn Stream<Item = Result<types::ChatEvent>> + Send>> {
    let mapped = stream.filter_map(|result| async move {
        match result {
            Err(e) => Some(Err(SynapseClientError::Llm(e.to_string()))),
            Ok(llm::StreamEvent::Done) => Some(Ok(types::ChatEvent::Done {
                finish_reason: None,
                usage: None,
            })),
            Ok(llm::StreamEvent::Usage(usage)) => Some(Ok(types::ChatEvent::Done {
                finish_reason: Some("stop".to_owned()),
                usage: Some(types::Usage {
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
                    total_tokens: usage.total_tokens,
                }),
            })),
            Ok(llm::StreamEvent::Delta(delta)) => {
                // Handle tool calls
                if let Some(ref tc) = delta.tool_call
                    && let Some(ref func) = tc.function
                {
                    if let (Some(id), Some(name)) = (&tc.id, &func.name) {
                        return Some(Ok(types::ChatEvent::ToolCallStart {
                            index: tc.index,
                            id: id.clone(),
                            name: name.clone(),
                        }));
                    } else if let Some(ref args) = func.arguments {
                        return Some(Ok(types::ChatEvent::ToolCallDelta {
                            index: tc.index,
                            arguments: args.clone(),
                        }));
                    }
                }

                // Handle text content
                if let Some(content) = delta.content {
                    return Some(Ok(types::ChatEvent::ContentDelta(content)));
                }

                None
            }
        }
    });

    Box::pin(mapped)
}

/// Convert an `LlmError` to a `SynapseClientError`
pub fn from_llm_error(e: &synapse_llm::LlmError) -> SynapseClientError {
    SynapseClientError::Llm(e.to_string())
}
