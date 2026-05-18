//! Conversion between internal types and `OpenAI` wire format

use crate::protocol::openai::{
    OpenAiChoice, OpenAiChoiceMessage, OpenAiContent, OpenAiContentPart, OpenAiFunction, OpenAiFunctionCall,
    OpenAiImageUrl, OpenAiMessage, OpenAiRequest, OpenAiResponse, OpenAiStreamChoice, OpenAiStreamChunk,
    OpenAiStreamDelta, OpenAiStreamFunctionCall, OpenAiStreamToolCall, OpenAiTool, OpenAiToolCall, OpenAiUsage,
};
use crate::types::{
    Choice, ChoiceMessage, CompletionParams, CompletionRequest, CompletionResponse, Content, ContentPart, FinishReason,
    FunctionCall, FunctionDefinition, Message, Role, StreamDelta, StreamEvent, StreamFunctionCall, StreamToolCall,
    ToolCall, ToolChoice, ToolChoiceFunction, ToolChoiceMode, ToolDefinition, Usage,
};

// -- Inbound: OpenAI wire format -> internal types --

impl From<OpenAiRequest> for CompletionRequest {
    fn from(req: OpenAiRequest) -> Self {
        Self {
            model: req.model,
            messages: req.messages.into_iter().map(Into::into).collect(),
            params: CompletionParams {
                temperature: req.temperature,
                top_p: req.top_p,
                max_tokens: req.max_tokens,
                stop: req.stop,
                frequency_penalty: req.frequency_penalty,
                presence_penalty: req.presence_penalty,
                seed: req.seed,
            },
            tools: req.tools.map(|tools| tools.into_iter().map(Into::into).collect()),
            tool_choice: req.tool_choice.and_then(|v| parse_openai_tool_choice(&v)),
            stream: req.stream.unwrap_or(false),
        }
    }
}

impl From<OpenAiMessage> for Message {
    fn from(msg: OpenAiMessage) -> Self {
        let role = match msg.role.as_str() {
            "system" => Role::System,
            "assistant" => Role::Assistant,
            "tool" => Role::Tool,
            _ => Role::User,
        };

        let content = match msg.content {
            Some(OpenAiContent::Text(text)) => Content::Text(text),
            Some(OpenAiContent::Parts(parts)) => Content::Parts(parts.into_iter().map(Into::into).collect()),
            None => Content::Text(String::new()),
        };

        let tool_calls = msg.tool_calls.map(|calls| {
            calls
                .into_iter()
                .map(|tc| ToolCall {
                    id: tc.id,
                    function: FunctionCall {
                        name: tc.function.name,
                        arguments: tc.function.arguments,
                    },
                })
                .collect()
        });

        Self {
            role,
            content,
            name: msg.name,
            tool_calls,
            tool_call_id: msg.tool_call_id,
        }
    }
}

impl From<OpenAiContentPart> for ContentPart {
    fn from(part: OpenAiContentPart) -> Self {
        match part {
            OpenAiContentPart::Text { text } => Self::Text { text },
            OpenAiContentPart::ImageUrl { image_url } => Self::Image {
                url: image_url.url,
                detail: image_url.detail,
            },
        }
    }
}

impl From<OpenAiTool> for ToolDefinition {
    fn from(tool: OpenAiTool) -> Self {
        Self {
            tool_type: tool.tool_type,
            function: FunctionDefinition {
                name: tool.function.name,
                description: tool.function.description,
                parameters: tool.function.parameters,
            },
        }
    }
}

/// Parse `OpenAI`'s flexible `tool_choice` field into our internal type
fn parse_openai_tool_choice(value: &serde_json::Value) -> Option<ToolChoice> {
    match value {
        serde_json::Value::String(s) => match s.as_str() {
            "none" => Some(ToolChoice::Mode(ToolChoiceMode::None)),
            "auto" => Some(ToolChoice::Mode(ToolChoiceMode::Auto)),
            "required" => Some(ToolChoice::Mode(ToolChoiceMode::Required)),
            _ => None,
        },
        serde_json::Value::Object(_) => serde_json::from_value::<ToolChoiceFunction>(value.clone())
            .ok()
            .map(ToolChoice::Function),
        _ => None,
    }
}

// -- Outbound: internal types -> OpenAI wire format --

impl From<CompletionResponse> for OpenAiResponse {
    fn from(resp: CompletionResponse) -> Self {
        Self {
            id: resp.id,
            object: resp.object,
            created: resp.created,
            model: resp.model,
            choices: resp.choices.into_iter().map(Into::into).collect(),
            usage: resp.usage.map(Into::into),
        }
    }
}

impl From<Choice> for OpenAiChoice {
    fn from(choice: Choice) -> Self {
        let finish_reason = choice.finish_reason.map(|fr| match fr {
            FinishReason::Stop => "stop".to_owned(),
            FinishReason::Length => "length".to_owned(),
            FinishReason::ToolCalls => "tool_calls".to_owned(),
            FinishReason::ContentFilter => "content_filter".to_owned(),
        });

        Self {
            index: choice.index,
            message: OpenAiChoiceMessage {
                role: choice.message.role,
                content: choice.message.content,
                tool_calls: choice.message.tool_calls.map(|calls| {
                    calls
                        .into_iter()
                        .map(|tc| OpenAiToolCall {
                            id: tc.id,
                            tool_type: "function".to_owned(),
                            function: OpenAiFunctionCall {
                                name: tc.function.name,
                                arguments: tc.function.arguments,
                            },
                        })
                        .collect()
                }),
            },
            finish_reason,
        }
    }
}

impl From<Usage> for OpenAiUsage {
    fn from(usage: Usage) -> Self {
        Self {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        }
    }
}

// -- Outbound: internal request -> OpenAI wire request (for sending to provider) --

impl From<&CompletionRequest> for OpenAiRequest {
    fn from(req: &CompletionRequest) -> Self {
        Self {
            model: req.model.clone(),
            messages: req.messages.iter().map(Into::into).collect(),
            temperature: req.params.temperature,
            top_p: req.params.top_p,
            max_tokens: req.params.max_tokens,
            stop: req.params.stop.clone(),
            frequency_penalty: req.params.frequency_penalty,
            presence_penalty: req.params.presence_penalty,
            seed: req.params.seed,
            stream: if req.stream { Some(true) } else { None },
            tools: req.tools.as_ref().map(|tools| {
                tools
                    .iter()
                    .map(|t| OpenAiTool {
                        tool_type: t.tool_type.clone(),
                        function: OpenAiFunction {
                            name: t.function.name.clone(),
                            description: t.function.description.clone(),
                            parameters: t.function.parameters.clone(),
                        },
                    })
                    .collect()
            }),
            tool_choice: req.tool_choice.as_ref().map(tool_choice_to_openai_value),
            stream_options: if req.stream {
                Some(crate::protocol::openai::OpenAiStreamOptions { include_usage: true })
            } else {
                None
            },
        }
    }
}

impl From<&Message> for OpenAiMessage {
    fn from(msg: &Message) -> Self {
        let role = match msg.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };

        let content = match &msg.content {
            Content::Text(text) => Some(OpenAiContent::Text(text.clone())),
            Content::Parts(parts) => Some(OpenAiContent::Parts(parts.iter().map(Into::into).collect())),
        };

        let tool_calls = msg.tool_calls.as_ref().map(|calls| {
            calls
                .iter()
                .map(|tc| OpenAiToolCall {
                    id: tc.id.clone(),
                    tool_type: "function".to_owned(),
                    function: OpenAiFunctionCall {
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    },
                })
                .collect()
        });

        Self {
            role: role.to_owned(),
            content,
            name: msg.name.clone(),
            tool_calls,
            tool_call_id: msg.tool_call_id.clone(),
        }
    }
}

impl From<&ContentPart> for OpenAiContentPart {
    fn from(part: &ContentPart) -> Self {
        match part {
            ContentPart::Text { text } => Self::Text { text: text.clone() },
            ContentPart::Image { url, detail } => Self::ImageUrl {
                image_url: OpenAiImageUrl {
                    url: url.clone(),
                    detail: detail.clone(),
                },
            },
        }
    }
}

/// Convert internal tool choice to `OpenAI` JSON value
fn tool_choice_to_openai_value(choice: &ToolChoice) -> serde_json::Value {
    match choice {
        ToolChoice::Mode(mode) => {
            let s = match mode {
                ToolChoiceMode::None => "none",
                ToolChoiceMode::Auto => "auto",
                ToolChoiceMode::Required => "required",
            };
            serde_json::Value::String(s.to_owned())
        }
        ToolChoice::Function(func) => {
            serde_json::json!({
                "type": func.tool_type,
                "function": {
                    "name": func.function.name
                }
            })
        }
    }
}

// -- Stream conversion --

/// Convert an `OpenAI` stream chunk into internal stream events
pub fn openai_chunk_to_events(chunk: &OpenAiStreamChunk) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    for choice in &chunk.choices {
        events.push(StreamEvent::Delta(openai_stream_choice_to_delta(choice)));
    }

    if let Some(usage) = &chunk.usage {
        events.push(StreamEvent::Usage(Usage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        }));
    }

    events
}

/// Convert an `OpenAI` stream choice to an internal stream delta
fn openai_stream_choice_to_delta(choice: &OpenAiStreamChoice) -> StreamDelta {
    let finish_reason = choice.finish_reason.as_deref().and_then(parse_finish_reason);

    let tool_call = choice
        .delta
        .tool_calls
        .as_ref()
        .and_then(|calls| calls.first())
        .map(|tc| StreamToolCall {
            index: tc.index,
            id: tc.id.clone(),
            function: tc.function.as_ref().map(|f| StreamFunctionCall {
                name: f.name.clone(),
                arguments: f.arguments.clone(),
            }),
        });

    StreamDelta {
        index: choice.index,
        content: choice.delta.content.clone(),
        tool_call,
        finish_reason,
    }
}

/// Convert an internal stream delta to an `OpenAI` stream chunk
pub fn delta_to_openai_chunk(delta: &StreamDelta, id: &str, model: &str, created: u64) -> OpenAiStreamChunk {
    let finish_reason = delta.finish_reason.as_ref().map(|fr| match fr {
        FinishReason::Stop => "stop".to_owned(),
        FinishReason::Length => "length".to_owned(),
        FinishReason::ToolCalls => "tool_calls".to_owned(),
        FinishReason::ContentFilter => "content_filter".to_owned(),
    });

    let tool_calls = delta.tool_call.as_ref().map(|tc| {
        vec![OpenAiStreamToolCall {
            index: tc.index,
            id: tc.id.clone(),
            tool_type: tc.id.as_ref().map(|_| "function".to_owned()),
            function: tc.function.as_ref().map(|f| OpenAiStreamFunctionCall {
                name: f.name.clone(),
                arguments: f.arguments.clone(),
            }),
        }]
    });

    OpenAiStreamChunk {
        id: id.to_owned(),
        object: "chat.completion.chunk".to_owned(),
        created,
        model: model.to_owned(),
        choices: vec![OpenAiStreamChoice {
            index: delta.index,
            delta: OpenAiStreamDelta {
                role: None,
                content: delta.content.clone(),
                tool_calls,
            },
            finish_reason,
        }],
        usage: None,
    }
}

/// Convert an internal `Usage` to an `OpenAI` stream chunk with usage data
pub fn usage_to_openai_chunk(usage: &Usage, id: &str, model: &str, created: u64) -> OpenAiStreamChunk {
    OpenAiStreamChunk {
        id: id.to_owned(),
        object: "chat.completion.chunk".to_owned(),
        created,
        model: model.to_owned(),
        choices: vec![],
        usage: Some(OpenAiUsage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        }),
    }
}

/// Parse an `OpenAI` response into internal types
impl From<OpenAiResponse> for CompletionResponse {
    fn from(resp: OpenAiResponse) -> Self {
        Self {
            id: resp.id,
            object: resp.object,
            created: resp.created,
            model: resp.model,
            choices: resp
                .choices
                .into_iter()
                .map(|c| {
                    let finish_reason = c.finish_reason.as_deref().and_then(parse_finish_reason);

                    let tool_calls = c.message.tool_calls.map(|calls| {
                        calls
                            .into_iter()
                            .map(|tc| ToolCall {
                                id: tc.id,
                                function: FunctionCall {
                                    name: tc.function.name,
                                    arguments: tc.function.arguments,
                                },
                            })
                            .collect()
                    });

                    Choice {
                        index: c.index,
                        message: ChoiceMessage {
                            role: c.message.role,
                            content: c.message.content,
                            tool_calls,
                        },
                        finish_reason,
                    }
                })
                .collect(),
            usage: resp.usage.map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }),
        }
    }
}

/// Parse a finish reason string
fn parse_finish_reason(s: &str) -> Option<FinishReason> {
    match s {
        "stop" | "end_turn" => Some(FinishReason::Stop),
        "length" | "max_tokens" => Some(FinishReason::Length),
        "tool_calls" | "tool_use" => Some(FinishReason::ToolCalls),
        "content_filter" => Some(FinishReason::ContentFilter),
        _ => None,
    }
}
