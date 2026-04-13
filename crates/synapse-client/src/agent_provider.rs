//! `agent-core::LlmProvider` implementation for `SynapseClient`
//!
//! Allows any agent-core consumer to use Synapse as an LLM provider

use async_trait::async_trait;
use futures::StreamExt;

use agent_core::error::AgentError;
use agent_core::provider::{CompletionEvent, CompletionRequest, CompletionStream, LlmProvider};
use agent_core::types::{Content, ContentBlock, Role, StopReason, Usage};

use crate::client::SynapseClient;
use crate::types::{ChatEvent, ChatRequest, FunctionCall, FunctionDefinition, Message, ToolCall, ToolDefinition};

impl From<crate::McpTool> for agent_core::types::Tool {
    fn from(t: crate::McpTool) -> Self {
        Self {
            name: t.name,
            description: t.description,
            input_schema: t.input_schema,
        }
    }
}

#[async_trait]
impl LlmProvider for SynapseClient {
    fn name(&self) -> &'static str {
        "synapse"
    }

    async fn stream(&self, request: CompletionRequest) -> agent_core::error::Result<CompletionStream> {
        let synapse_req = to_chat_request(&request);

        let stream = self
            .chat_completion_stream(&synapse_req)
            .await
            .map_err(|e| AgentError::Api {
                status: 0,
                message: e.to_string(),
            })?;

        let mapped = stream.map(|result| {
            result.map(to_completion_event).map_err(|e| AgentError::Api {
                status: 0,
                message: e.to_string(),
            })
        });

        Ok(Box::pin(mapped))
    }
}

/// Convert an agent-core `CompletionRequest` to a synapse-client `ChatRequest`
fn to_chat_request(req: &CompletionRequest) -> ChatRequest {
    let mut messages = Vec::new();

    if let Some(ref system) = req.system {
        messages.push(Message::system(system));
    }

    for msg in &req.messages {
        match msg.role {
            Role::User => match &msg.content {
                Content::Text(text) => {
                    messages.push(Message::user(text));
                }
                Content::Blocks(blocks) => {
                    // Check for tool results — each becomes its own message
                    for block in blocks {
                        match block {
                            ContentBlock::ToolResult {
                                tool_use_id, content, ..
                            } => {
                                messages.push(Message::tool(tool_use_id, content));
                            }
                            ContentBlock::Text { text } => {
                                messages.push(Message::user(text));
                            }
                            _ => {}
                        }
                    }
                }
            },
            Role::Assistant => match &msg.content {
                Content::Text(text) => {
                    messages.push(Message::assistant(text));
                }
                Content::Blocks(blocks) => {
                    // Collect text and tool_calls from blocks
                    let mut text_parts = Vec::new();
                    let mut tool_calls = Vec::new();

                    for block in blocks {
                        match block {
                            ContentBlock::Text { text } => {
                                text_parts.push(text.as_str());
                            }
                            ContentBlock::ToolUse { id, name, input } => {
                                tool_calls.push(ToolCall {
                                    id: id.clone(),
                                    tool_type: "function".to_owned(),
                                    function: FunctionCall {
                                        name: name.clone(),
                                        arguments: input.to_string(),
                                    },
                                });
                            }
                            _ => {}
                        }
                    }

                    let content_text = text_parts.join("");
                    let content_val = if content_text.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(content_text)
                    };

                    messages.push(Message {
                        role: "assistant".to_owned(),
                        content: content_val,
                        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                        tool_call_id: None,
                    });
                }
            },
        }
    }

    ChatRequest {
        model: req.model.clone(),
        messages,
        stream: true,
        temperature: None,
        top_p: None,
        max_tokens: Some(req.max_tokens),
        stop: None,
        tools: req.tools.as_ref().map(|tools| {
            tools
                .iter()
                .map(|t| ToolDefinition {
                    tool_type: "function".to_owned(),
                    function: FunctionDefinition {
                        name: t.name.clone(),
                        description: Some(t.description.clone()),
                        parameters: Some(t.input_schema.clone()),
                    },
                })
                .collect()
        }),
        tool_choice: None,
    }
}

/// Convert a synapse-client `ChatEvent` to an agent-core `CompletionEvent`
fn to_completion_event(event: ChatEvent) -> CompletionEvent {
    match event {
        ChatEvent::ContentDelta(text) => CompletionEvent::TextDelta(text),
        ChatEvent::ToolCallStart { index, id, name } => CompletionEvent::ToolUseStart {
            index: index as usize,
            id,
            name,
        },
        ChatEvent::ToolCallDelta { index, arguments } => CompletionEvent::ToolInputDelta {
            index: index as usize,
            partial_json: arguments,
        },
        ChatEvent::Done { finish_reason, usage } => CompletionEvent::Done {
            stop_reason: finish_reason.map(|r| match r.as_str() {
                "stop" => StopReason::EndTurn,
                "tool_calls" => StopReason::ToolUse,
                "length" => StopReason::MaxTokens,
                _ => StopReason::EndTurn,
            }),
            usage: usage.map(|u| Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
            }),
        },
        ChatEvent::Error(msg) => CompletionEvent::Error(msg),
    }
}
