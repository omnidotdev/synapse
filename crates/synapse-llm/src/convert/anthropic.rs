//! Conversion between internal types and Anthropic wire format

use std::time::{SystemTime, UNIX_EPOCH};

use crate::protocol::anthropic::{
    AnthropicContent, AnthropicContentBlock, AnthropicImageSource, AnthropicMessage, AnthropicMessageDelta,
    AnthropicRequest, AnthropicResponse, AnthropicResponseBlock, AnthropicStreamContentBlock, AnthropicStreamDelta,
    AnthropicStreamEvent, AnthropicTool, AnthropicToolChoice, AnthropicUsage,
};
use crate::types::{
    Choice, ChoiceMessage, CompletionParams, CompletionRequest, CompletionResponse, Content, ContentPart, FinishReason,
    FunctionCall, FunctionDefinition, Message, Role, StreamDelta, StreamEvent, StreamFunctionCall, StreamToolCall,
    ToolCall, ToolChoice, ToolChoiceFunction, ToolChoiceMode, ToolDefinition, Usage,
};

/// Default max tokens when not specified (Anthropic requires this field)
const DEFAULT_MAX_TOKENS: u32 = 4096;

// -- Inbound: Anthropic wire format -> internal types --

impl From<AnthropicRequest> for CompletionRequest {
    fn from(req: AnthropicRequest) -> Self {
        let mut messages: Vec<Message> = Vec::new();

        // Convert system prompt to a system message
        if let Some(system) = req.system {
            messages.push(Message {
                role: Role::System,
                content: Content::Text(system),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            });
        }

        // Convert Anthropic messages
        for msg in req.messages {
            messages.push(anthropic_message_to_internal(msg));
        }

        Self {
            model: req.model,
            messages,
            params: CompletionParams {
                temperature: req.temperature,
                top_p: req.top_p,
                max_tokens: Some(req.max_tokens),
                stop: req.stop_sequences,
                frequency_penalty: None,
                presence_penalty: None,
                seed: None,
            },
            tools: req.tools.map(|tools| tools.into_iter().map(Into::into).collect()),
            tool_choice: req.tool_choice.map(|tc| anthropic_tool_choice_to_internal(&tc)),
            stream: req.stream.unwrap_or(false),
        }
    }
}

/// Convert a single Anthropic message to internal representation
fn anthropic_message_to_internal(msg: AnthropicMessage) -> Message {
    let role = match msg.role.as_str() {
        "assistant" => Role::Assistant,
        _ => Role::User,
    };

    match msg.content {
        AnthropicContent::Text(text) => Message {
            role,
            content: Content::Text(text),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        },
        AnthropicContent::Blocks(blocks) => {
            let mut text_parts = Vec::new();
            let mut tool_calls = Vec::new();
            let mut tool_call_id = None;
            let mut tool_result_content = None;

            for block in blocks {
                match block {
                    AnthropicContentBlock::Text { text } => {
                        text_parts.push(ContentPart::Text { text });
                    }
                    AnthropicContentBlock::Image { source } => {
                        // Convert to internal image representation
                        let url = if source.source_type == "base64" {
                            let mime = source.media_type.unwrap_or_else(|| "image/png".to_owned());
                            format!("data:{mime};base64,{}", source.data)
                        } else {
                            source.data
                        };
                        text_parts.push(ContentPart::Image { url, detail: None });
                    }
                    AnthropicContentBlock::ToolUse { id, name, input } => {
                        let arguments = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_owned());
                        tool_calls.push(ToolCall {
                            id,
                            function: FunctionCall { name, arguments },
                        });
                    }
                    AnthropicContentBlock::ToolResult {
                        tool_use_id, content, ..
                    } => {
                        tool_call_id = Some(tool_use_id);
                        tool_result_content = content;
                    }
                }
            }

            // If this is a tool result message, return it as such
            if let Some(tc_id) = tool_call_id {
                return Message {
                    role: Role::Tool,
                    content: Content::Text(tool_result_content.unwrap_or_default()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: Some(tc_id),
                };
            }

            let content = if text_parts.len() == 1 {
                match text_parts.into_iter().next() {
                    Some(ContentPart::Text { text }) => Content::Text(text),
                    Some(other) => Content::Parts(vec![other]),
                    None => Content::Text(String::new()),
                }
            } else if text_parts.is_empty() {
                Content::Text(String::new())
            } else {
                Content::Parts(text_parts)
            };

            Message {
                role,
                content,
                name: None,
                tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                tool_call_id: None,
            }
        }
    }
}

impl From<AnthropicTool> for ToolDefinition {
    fn from(tool: AnthropicTool) -> Self {
        Self {
            tool_type: "function".to_owned(),
            function: FunctionDefinition {
                name: tool.name,
                description: tool.description,
                parameters: Some(tool.input_schema),
            },
        }
    }
}

/// Convert Anthropic tool choice to internal representation
fn anthropic_tool_choice_to_internal(tc: &AnthropicToolChoice) -> ToolChoice {
    match tc.choice_type.as_str() {
        "any" => ToolChoice::Mode(ToolChoiceMode::Required),
        "tool" => tc.name.as_ref().map_or(ToolChoice::Mode(ToolChoiceMode::Auto), |name| {
            ToolChoice::Function(ToolChoiceFunction {
                tool_type: "function".to_owned(),
                function: crate::types::ToolChoiceFunctionName { name: name.clone() },
            })
        }),
        // "auto" and unknown types both default to Auto
        _ => ToolChoice::Mode(ToolChoiceMode::Auto),
    }
}

// -- Outbound: internal types -> Anthropic wire format --

/// Convert an internal `CompletionRequest` to Anthropic wire format
impl From<&CompletionRequest> for AnthropicRequest {
    fn from(req: &CompletionRequest) -> Self {
        let mut system = None;
        let mut messages = Vec::new();

        for msg in &req.messages {
            match msg.role {
                Role::System => {
                    system = Some(msg.content.as_text());
                }
                _ => {
                    messages.push(internal_message_to_anthropic(msg));
                }
            }
        }

        let tools = req.tools.as_ref().map(|tools| {
            tools
                .iter()
                .map(|t| AnthropicTool {
                    name: t.function.name.clone(),
                    description: t.function.description.clone(),
                    input_schema: t
                        .function
                        .parameters
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({"type": "object"})),
                })
                .collect()
        });

        let tool_choice = req.tool_choice.as_ref().map(internal_tool_choice_to_anthropic);

        Self {
            model: req.model.clone(),
            max_tokens: req.params.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            system,
            messages,
            temperature: req.params.temperature,
            top_p: req.params.top_p,
            top_k: None,
            stop_sequences: req.params.stop.clone(),
            stream: if req.stream { Some(true) } else { None },
            tools,
            tool_choice,
        }
    }
}

/// Convert an internal message to Anthropic wire format
fn internal_message_to_anthropic(msg: &Message) -> AnthropicMessage {
    let role = match msg.role {
        Role::Assistant => "assistant",
        Role::Tool | Role::User | Role::System => "user",
    };

    // Handle tool result messages
    if msg.role == Role::Tool
        && let Some(tool_call_id) = &msg.tool_call_id
    {
        return AnthropicMessage {
            role: "user".to_owned(),
            content: AnthropicContent::Blocks(vec![AnthropicContentBlock::ToolResult {
                tool_use_id: tool_call_id.clone(),
                content: Some(msg.content.as_text()),
                is_error: None,
            }]),
        };
    }

    // Handle assistant messages with tool calls
    if let Some(tool_calls) = &msg.tool_calls {
        let mut blocks: Vec<AnthropicContentBlock> = Vec::new();

        let text = msg.content.as_text();
        if !text.is_empty() {
            blocks.push(AnthropicContentBlock::Text { text });
        }

        for tc in tool_calls {
            let input = serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| serde_json::json!({}));
            blocks.push(AnthropicContentBlock::ToolUse {
                id: tc.id.clone(),
                name: tc.function.name.clone(),
                input,
            });
        }

        return AnthropicMessage {
            role: role.to_owned(),
            content: AnthropicContent::Blocks(blocks),
        };
    }

    // Handle regular content
    let content = match &msg.content {
        Content::Text(text) => AnthropicContent::Text(text.clone()),
        Content::Parts(parts) => {
            let blocks = parts
                .iter()
                .map(|part| match part {
                    ContentPart::Text { text } => AnthropicContentBlock::Text { text: text.clone() },
                    ContentPart::Image { url, .. } => {
                        // Parse data URI or use URL directly
                        if let Some(rest) = url.strip_prefix("data:")
                            && let Some((mime_and_encoding, data)) = rest.split_once(',')
                        {
                            let media_type = mime_and_encoding.strip_suffix(";base64").unwrap_or(mime_and_encoding);
                            AnthropicContentBlock::Image {
                                source: AnthropicImageSource {
                                    source_type: "base64".to_owned(),
                                    media_type: Some(media_type.to_owned()),
                                    data: data.to_owned(),
                                },
                            }
                        } else {
                            AnthropicContentBlock::Image {
                                source: AnthropicImageSource {
                                    source_type: "url".to_owned(),
                                    media_type: None,
                                    data: url.clone(),
                                },
                            }
                        }
                    }
                })
                .collect();
            AnthropicContent::Blocks(blocks)
        }
    };

    AnthropicMessage {
        role: role.to_owned(),
        content,
    }
}

/// Convert internal tool choice to Anthropic wire format
fn internal_tool_choice_to_anthropic(choice: &ToolChoice) -> AnthropicToolChoice {
    match choice {
        ToolChoice::Mode(mode) => match mode {
            // Anthropic has no "none" mode; map both None and Auto to "auto"
            ToolChoiceMode::None | ToolChoiceMode::Auto => AnthropicToolChoice {
                choice_type: "auto".to_owned(),
                name: None,
            },
            ToolChoiceMode::Required => AnthropicToolChoice {
                choice_type: "any".to_owned(),
                name: None,
            },
        },
        ToolChoice::Function(func) => AnthropicToolChoice {
            choice_type: "tool".to_owned(),
            name: Some(func.function.name.clone()),
        },
    }
}

// -- Response conversion: Anthropic -> internal --

impl From<AnthropicResponse> for CompletionResponse {
    fn from(resp: AnthropicResponse) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let mut text_content = String::new();
        let mut tool_calls = Vec::new();

        for block in &resp.content {
            match block {
                AnthropicResponseBlock::Text { text } => {
                    text_content.push_str(text);
                }
                AnthropicResponseBlock::ToolUse { id, name, input } => {
                    let arguments = serde_json::to_string(input).unwrap_or_else(|_| "{}".to_owned());
                    tool_calls.push(ToolCall {
                        id: id.clone(),
                        function: FunctionCall {
                            name: name.clone(),
                            arguments,
                        },
                    });
                }
            }
        }

        let finish_reason = resp.stop_reason.as_deref().and_then(|s| match s {
            "end_turn" | "stop" => Some(FinishReason::Stop),
            "max_tokens" => Some(FinishReason::Length),
            "tool_use" => Some(FinishReason::ToolCalls),
            _ => None,
        });

        let message = if tool_calls.is_empty() {
            ChoiceMessage {
                role: "assistant".to_owned(),
                content: Some(text_content),
                tool_calls: None,
            }
        } else {
            ChoiceMessage {
                role: "assistant".to_owned(),
                content: if text_content.is_empty() {
                    None
                } else {
                    Some(text_content)
                },
                tool_calls: Some(tool_calls),
            }
        };

        Self {
            id: resp.id,
            object: "chat.completion".to_owned(),
            created: now,
            model: resp.model,
            choices: vec![Choice {
                index: 0,
                message,
                finish_reason,
            }],
            usage: Some(Usage {
                prompt_tokens: resp.usage.input_tokens,
                completion_tokens: resp.usage.output_tokens,
                total_tokens: resp.usage.input_tokens + resp.usage.output_tokens,
            }),
        }
    }
}

// -- Outbound: internal response -> Anthropic wire format --

impl From<CompletionResponse> for AnthropicResponse {
    fn from(resp: CompletionResponse) -> Self {
        let choice = resp.choices.into_iter().next();

        let mut content = Vec::new();
        if let Some(ref c) = choice {
            if let Some(text) = &c.message.content {
                content.push(AnthropicResponseBlock::Text { text: text.clone() });
            }
            if let Some(tool_calls) = &c.message.tool_calls {
                for tc in tool_calls {
                    let input = serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| serde_json::json!({}));
                    content.push(AnthropicResponseBlock::ToolUse {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        input,
                    });
                }
            }
        }

        let stop_reason = choice
            .as_ref()
            .and_then(|c| c.finish_reason.as_ref())
            .map(|fr| match fr {
                // Anthropic has no content_filter reason; map both to end_turn
                FinishReason::Stop | FinishReason::ContentFilter => "end_turn".to_owned(),
                FinishReason::Length => "max_tokens".to_owned(),
                FinishReason::ToolCalls => "tool_use".to_owned(),
            });

        let usage = resp.usage.unwrap_or_default();

        Self {
            id: resp.id,
            response_type: "message".to_owned(),
            role: "assistant".to_owned(),
            content,
            model: resp.model,
            stop_reason,
            stop_sequence: None,
            usage: AnthropicUsage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
            },
        }
    }
}

// -- Stream conversion --

/// State tracker for converting Anthropic stream events
#[derive(Debug, Default)]
pub struct AnthropicStreamState {
    /// Current content block index being streamed
    current_block_index: u32,
    /// Current tool call info (id, name) for `tool_use` blocks
    current_tool: Option<(String, String)>,
    /// Whether we've started accumulating tool input JSON
    tool_json_started: bool,
    /// Sequential 0-based index of the tool call currently being streamed
    ///
    /// Anthropic's content block index is shared across all block types (text,
    /// `tool_use`, …), so it cannot be used as the tool-call index — a tool use
    /// that follows a text block would have `content_block` index 1+, creating
    /// phantom entries in consumers that index by this value.
    current_tool_call_index: u32,
    /// Counter used to assign the next tool call its sequential index
    next_tool_call_index: u32,
}

impl AnthropicStreamState {
    /// Create a new stream state tracker
    pub fn new() -> Self {
        Self::default()
    }

    /// Convert an Anthropic stream event to internal stream events
    pub fn convert_event(&mut self, event: &AnthropicStreamEvent) -> Vec<StreamEvent> {
        match event {
            AnthropicStreamEvent::MessageStart { .. } | AnthropicStreamEvent::Ping => Vec::new(),

            AnthropicStreamEvent::ContentBlockStart { index, content_block } => {
                self.current_block_index = *index;
                match content_block {
                    AnthropicStreamContentBlock::Text { .. } => Vec::new(),
                    AnthropicStreamContentBlock::ToolUse { id, name, .. } => {
                        self.current_tool = Some((id.clone(), name.clone()));
                        self.current_tool_call_index = self.next_tool_call_index;
                        self.next_tool_call_index += 1;
                        self.tool_json_started = false;
                        vec![StreamEvent::Delta(StreamDelta {
                            index: 0,
                            content: None,
                            tool_call: Some(StreamToolCall {
                                index: self.current_tool_call_index,
                                id: Some(id.clone()),
                                function: Some(StreamFunctionCall {
                                    name: Some(name.clone()),
                                    arguments: None,
                                }),
                            }),
                            finish_reason: None,
                        })]
                    }
                }
            }

            AnthropicStreamEvent::ContentBlockDelta { delta, .. } => match delta {
                AnthropicStreamDelta::TextDelta { text } => {
                    vec![StreamEvent::Delta(StreamDelta {
                        index: 0,
                        content: Some(text.clone()),
                        tool_call: None,
                        finish_reason: None,
                    })]
                }
                AnthropicStreamDelta::InputJsonDelta { partial_json } => {
                    self.tool_json_started = true;
                    vec![StreamEvent::Delta(StreamDelta {
                        index: 0,
                        content: None,
                        tool_call: Some(StreamToolCall {
                            index: self.current_tool_call_index,
                            id: None,
                            function: Some(StreamFunctionCall {
                                name: None,
                                arguments: Some(partial_json.clone()),
                            }),
                        }),
                        finish_reason: None,
                    })]
                }
            },

            AnthropicStreamEvent::ContentBlockStop { .. } => {
                self.current_tool = None;
                self.tool_json_started = false;
                Vec::new()
            }

            AnthropicStreamEvent::MessageDelta { delta, usage } => {
                let mut events = Vec::new();

                let finish_reason = delta.stop_reason.as_deref().and_then(|s| match s {
                    "end_turn" | "stop" => Some(FinishReason::Stop),
                    "max_tokens" => Some(FinishReason::Length),
                    "tool_use" => Some(FinishReason::ToolCalls),
                    _ => None,
                });

                if finish_reason.is_some() {
                    events.push(StreamEvent::Delta(StreamDelta {
                        index: 0,
                        content: None,
                        tool_call: None,
                        finish_reason,
                    }));
                }

                if let Some(usage) = usage {
                    events.push(StreamEvent::Usage(Usage {
                        prompt_tokens: usage.input_tokens,
                        completion_tokens: usage.output_tokens,
                        total_tokens: usage.input_tokens + usage.output_tokens,
                    }));
                }

                events
            }

            AnthropicStreamEvent::MessageStop => {
                vec![StreamEvent::Done]
            }
        }
    }
}

/// Build Anthropic stream events from internal stream events (for Anthropic-compatible output)
pub fn internal_to_anthropic_stream_events(
    event: &StreamEvent,
    _model: &str,
    _response_id: &str,
) -> Vec<AnthropicStreamEvent> {
    match event {
        StreamEvent::Delta(delta) => {
            let mut events = Vec::new();

            if let Some(content) = &delta.content {
                events.push(AnthropicStreamEvent::ContentBlockDelta {
                    index: 0,
                    delta: AnthropicStreamDelta::TextDelta { text: content.clone() },
                });
            }

            if let Some(tc) = &delta.tool_call
                && let Some(func) = &tc.function
                && let Some(args) = &func.arguments
            {
                events.push(AnthropicStreamEvent::ContentBlockDelta {
                    index: tc.index,
                    delta: AnthropicStreamDelta::InputJsonDelta {
                        partial_json: args.clone(),
                    },
                });
            }

            if let Some(finish_reason) = &delta.finish_reason {
                let stop_reason = match finish_reason {
                    FinishReason::Stop | FinishReason::ContentFilter => "end_turn",
                    FinishReason::Length => "max_tokens",
                    FinishReason::ToolCalls => "tool_use",
                };

                events.push(AnthropicStreamEvent::MessageDelta {
                    delta: AnthropicMessageDelta {
                        stop_reason: Some(stop_reason.to_owned()),
                        stop_sequence: None,
                    },
                    usage: None,
                });
            }

            events
        }
        StreamEvent::Usage(usage) => {
            vec![AnthropicStreamEvent::MessageDelta {
                delta: AnthropicMessageDelta {
                    stop_reason: None,
                    stop_sequence: None,
                },
                usage: Some(AnthropicUsage {
                    input_tokens: usage.prompt_tokens,
                    output_tokens: usage.completion_tokens,
                }),
            }]
        }
        StreamEvent::Done => {
            vec![AnthropicStreamEvent::MessageStop]
        }
    }
}
