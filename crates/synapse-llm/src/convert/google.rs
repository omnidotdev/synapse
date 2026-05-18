//! Conversion between internal types and Google Generative Language wire format

use std::time::{SystemTime, UNIX_EPOCH};

use crate::protocol::google::{
    GoogleCandidate, GoogleContent, GoogleFunctionCall, GoogleFunctionCallingConfig, GoogleFunctionDeclaration,
    GoogleFunctionResponse, GoogleGenerationConfig, GoogleInlineData, GooglePart, GoogleRequest, GoogleResponse,
    GoogleTool, GoogleToolConfig,
};
use crate::types::{
    Choice, ChoiceMessage, CompletionRequest, CompletionResponse, Content, ContentPart, FinishReason, FunctionCall,
    Message, Role, StreamDelta, StreamEvent, StreamFunctionCall, StreamToolCall, ToolCall, ToolChoice, ToolChoiceMode,
    Usage,
};

// -- Outbound: internal request -> Google wire request --

impl From<&CompletionRequest> for GoogleRequest {
    fn from(req: &CompletionRequest) -> Self {
        let mut system_instruction = None;
        let mut contents = Vec::new();

        for msg in &req.messages {
            match msg.role {
                Role::System => {
                    system_instruction = Some(GoogleContent {
                        role: None,
                        parts: vec![GooglePart::Text(msg.content.as_text())],
                    });
                }
                Role::User => {
                    contents.push(internal_message_to_google(msg, "user"));
                }
                Role::Assistant => {
                    contents.push(internal_message_to_google(msg, "model"));
                }
                Role::Tool => {
                    // Tool results become function responses
                    if let Some(tool_call_id) = &msg.tool_call_id {
                        let response_value = serde_json::from_str(&msg.content.as_text())
                            .unwrap_or_else(|_| serde_json::json!({"result": msg.content.as_text()}));
                        contents.push(GoogleContent {
                            role: Some("function".to_owned()),
                            parts: vec![GooglePart::FunctionResponse(GoogleFunctionResponse {
                                name: tool_call_id.clone(),
                                response: response_value,
                            })],
                        });
                    }
                }
            }
        }

        let generation_config = Some(GoogleGenerationConfig {
            temperature: req.params.temperature,
            top_p: req.params.top_p,
            top_k: None,
            max_output_tokens: req.params.max_tokens,
            stop_sequences: req.params.stop.clone(),
            candidate_count: None,
        });

        let tools = req.tools.as_ref().map(|tools| {
            vec![GoogleTool {
                function_declarations: tools
                    .iter()
                    .map(|t| GoogleFunctionDeclaration {
                        name: t.function.name.clone(),
                        description: t.function.description.clone(),
                        parameters: t.function.parameters.clone(),
                    })
                    .collect(),
            }]
        });

        let tool_config = req.tool_choice.as_ref().map(|tc| {
            let (mode, allowed_names) = match tc {
                ToolChoice::Mode(ToolChoiceMode::None) => ("NONE".to_owned(), None),
                ToolChoice::Mode(ToolChoiceMode::Auto) => ("AUTO".to_owned(), None),
                ToolChoice::Mode(ToolChoiceMode::Required) => ("ANY".to_owned(), None),
                ToolChoice::Function(func) => ("ANY".to_owned(), Some(vec![func.function.name.clone()])),
            };
            GoogleToolConfig {
                function_calling_config: GoogleFunctionCallingConfig {
                    mode,
                    allowed_function_names: allowed_names,
                },
            }
        });

        Self {
            contents,
            system_instruction,
            generation_config,
            tools,
            tool_config,
        }
    }
}

/// Convert an internal message to a Google content object
fn internal_message_to_google(msg: &Message, role: &str) -> GoogleContent {
    let mut parts = Vec::new();

    match &msg.content {
        Content::Text(text) => {
            if !text.is_empty() {
                parts.push(GooglePart::Text(text.clone()));
            }
        }
        Content::Parts(content_parts) => {
            for part in content_parts {
                match part {
                    ContentPart::Text { text } => {
                        parts.push(GooglePart::Text(text.clone()));
                    }
                    ContentPart::Image { url, .. } => {
                        // Parse data URI for inline data
                        if let Some(rest) = url.strip_prefix("data:")
                            && let Some((mime_and_encoding, data)) = rest.split_once(',')
                        {
                            let mime_type = mime_and_encoding.strip_suffix(";base64").unwrap_or(mime_and_encoding);
                            parts.push(GooglePart::InlineData(GoogleInlineData {
                                mime_type: mime_type.to_owned(),
                                data: data.to_owned(),
                            }));
                        }
                        // Google doesn't support URL-based images in the same way;
                        // skip non-data-URI images
                    }
                }
            }
        }
    }

    // Add tool calls as function calls
    if let Some(tool_calls) = &msg.tool_calls {
        for tc in tool_calls {
            let args = serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| serde_json::json!({}));
            parts.push(GooglePart::FunctionCall(GoogleFunctionCall {
                name: tc.function.name.clone(),
                args,
            }));
        }
    }

    // Ensure at least one part
    if parts.is_empty() {
        parts.push(GooglePart::Text(String::new()));
    }

    GoogleContent {
        role: Some(role.to_owned()),
        parts,
    }
}

// -- Inbound: Google wire response -> internal types --

impl From<GoogleResponse> for CompletionResponse {
    fn from(resp: GoogleResponse) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        #[allow(clippy::cast_possible_truncation)]
        let choices = resp
            .candidates
            .into_iter()
            .enumerate()
            .map(|(i, candidate)| google_candidate_to_choice(&candidate, i as u32))
            .collect();

        let usage = resp.usage_metadata.map(|u| Usage {
            prompt_tokens: u.prompt_token_count,
            completion_tokens: u.candidates_token_count,
            total_tokens: u.total_token_count,
        });

        Self {
            id: format!("google-{now}"),
            object: "chat.completion".to_owned(),
            created: now,
            model: String::new(), // Filled in by the provider
            choices,
            usage,
        }
    }
}

/// Convert a Google candidate to an internal choice
fn google_candidate_to_choice(candidate: &GoogleCandidate, default_index: u32) -> Choice {
    let index = candidate.index.unwrap_or(default_index);

    let mut text_content = String::new();
    let mut tool_calls = Vec::new();

    for part in &candidate.content.parts {
        match part {
            GooglePart::Text(text) => text_content.push_str(text.as_str()),
            GooglePart::FunctionCall(fc) => {
                let arguments = serde_json::to_string(&fc.args).unwrap_or_else(|_| "{}".to_owned());
                tool_calls.push(ToolCall {
                    id: format!("call_{}", fc.name),
                    function: FunctionCall {
                        name: fc.name.clone(),
                        arguments,
                    },
                });
            }
            _ => {}
        }
    }

    let finish_reason = candidate.finish_reason.as_deref().and_then(|s| match s {
        "STOP" => Some(FinishReason::Stop),
        "MAX_TOKENS" => Some(FinishReason::Length),
        "SAFETY" => Some(FinishReason::ContentFilter),
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

    Choice {
        index,
        message,
        finish_reason,
    }
}

// -- Stream conversion --

/// Convert a Google streaming chunk to internal stream events
pub fn google_chunk_to_events(chunk: &GoogleResponse) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    for (i, candidate) in chunk.candidates.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let index = candidate.index.unwrap_or(i as u32);

        for part in &candidate.content.parts {
            match part {
                GooglePart::Text(text) => {
                    events.push(StreamEvent::Delta(StreamDelta {
                        index,
                        content: Some(text.clone()),
                        tool_call: None,
                        finish_reason: None,
                    }));
                }
                GooglePart::FunctionCall(fc) => {
                    let arguments = serde_json::to_string(&fc.args).unwrap_or_else(|_| "{}".to_owned());
                    events.push(StreamEvent::Delta(StreamDelta {
                        index,
                        content: None,
                        tool_call: Some(StreamToolCall {
                            index: 0,
                            id: Some(format!("call_{}", fc.name)),
                            function: Some(StreamFunctionCall {
                                name: Some(fc.name.clone()),
                                arguments: Some(arguments),
                            }),
                        }),
                        finish_reason: None,
                    }));
                }
                _ => {}
            }
        }

        let finish_reason = candidate.finish_reason.as_deref().and_then(|s| match s {
            "STOP" => Some(FinishReason::Stop),
            "MAX_TOKENS" => Some(FinishReason::Length),
            "SAFETY" => Some(FinishReason::ContentFilter),
            _ => None,
        });

        if finish_reason.is_some() {
            events.push(StreamEvent::Delta(StreamDelta {
                index,
                content: None,
                tool_call: None,
                finish_reason,
            }));
        }
    }

    if let Some(usage) = &chunk.usage_metadata {
        events.push(StreamEvent::Usage(Usage {
            prompt_tokens: usage.prompt_token_count,
            completion_tokens: usage.candidates_token_count,
            total_tokens: usage.total_token_count,
        }));
    }

    events
}
