//! AWS Bedrock provider implementation using the Converse API

use std::pin::Pin;

use async_trait::async_trait;
use aws_sdk_bedrockruntime::Client as BedrockClient;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, ConverseOutput, InferenceConfiguration, Message as BedrockMessage,
    SystemContentBlock, Tool, ToolConfiguration, ToolInputSchema, ToolResultBlock, ToolResultContentBlock,
    ToolSpecification, ToolUseBlock,
};
use futures_util::{Stream, StreamExt};
use secrecy::ExposeSecret;
use synapse_config::{LlmProviderConfig, LlmProviderType};
use synapse_core::RequestContext;

use super::{Provider, ProviderCapabilities};
use crate::error::LlmError;
use crate::types::{
    Choice, ChoiceMessage, CompletionRequest, CompletionResponse, Content, FinishReason, FunctionCall, Message, Role,
    StreamDelta, StreamEvent, StreamFunctionCall, StreamToolCall, ToolCall, Usage,
};

/// AWS Bedrock provider using the Converse API
pub struct BedrockProvider {
    name: String,
    client: BedrockClient,
}

impl BedrockProvider {
    /// Create from provider configuration
    ///
    /// # Errors
    ///
    /// Returns `LlmError::Internal` if AWS configuration fails.
    pub async fn new(name: String, config: &LlmProviderConfig) -> Result<Self, LlmError> {
        let LlmProviderType::Bedrock(bedrock_config) = &config.provider_type else {
            return Err(LlmError::Internal(anyhow::anyhow!("expected bedrock provider type")));
        };

        let client = build_bedrock_client(bedrock_config).await?;

        Ok(Self { name, client })
    }
}

/// Build a Bedrock runtime client from configuration
async fn build_bedrock_client(config: &synapse_config::BedrockConfig) -> Result<BedrockClient, LlmError> {
    let mut aws_config_builder = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(config.region.clone()));

    // Use explicit credentials if provided, otherwise fall back to default chain
    if let (Some(access_key), Some(secret_key)) = (&config.access_key_id, &config.secret_access_key) {
        let credentials = aws_credential_types::Credentials::new(
            access_key.expose_secret(),
            secret_key.expose_secret(),
            None, // session token
            None, // expiry
            "synapse-config",
        );
        aws_config_builder = aws_config_builder.credentials_provider(credentials);
    }

    let aws_config = aws_config_builder.load().await;
    let client = BedrockClient::new(&aws_config);

    Ok(client)
}

#[async_trait]
impl Provider for BedrockProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
        }
    }

    async fn complete(
        &self,
        request: &CompletionRequest,
        _context: &RequestContext,
    ) -> Result<CompletionResponse, LlmError> {
        let (system_blocks, messages) = build_converse_input(request)?;

        let mut converse = self.client.converse().model_id(&request.model);

        for block in &system_blocks {
            converse = converse.system(block.clone());
        }

        for msg in &messages {
            converse = converse.messages(msg.clone());
        }

        converse = converse.inference_config(build_inference_config(request));

        // Set tool configuration
        if let Some(tool_config) = build_tool_config(request) {
            converse = converse.tool_config(tool_config);
        }

        let output = converse.send().await.map_err(|e| {
            tracing::error!(provider = %self.name, error = %e, "bedrock converse failed");
            LlmError::Upstream(e.to_string())
        })?;

        let finish_reason = match output.stop_reason() {
            aws_sdk_bedrockruntime::types::StopReason::MaxTokens => Some(FinishReason::Length),
            aws_sdk_bedrockruntime::types::StopReason::ToolUse => Some(FinishReason::ToolCalls),
            aws_sdk_bedrockruntime::types::StopReason::ContentFiltered => Some(FinishReason::ContentFilter),
            // EndTurn and unknown variants default to Stop
            _ => Some(FinishReason::Stop),
        };

        let (content_text, tool_calls) = match output.output() {
            Some(ConverseOutput::Message(msg)) => extract_bedrock_response(msg),
            _ => (Some(String::new()), None),
        };

        #[allow(clippy::cast_sign_loss)]
        let usage = output.usage().map(|u| Usage {
            prompt_tokens: u.input_tokens() as u32,
            completion_tokens: u.output_tokens() as u32,
            total_tokens: u.total_tokens() as u32,
        });

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        Ok(CompletionResponse {
            id: format!("bedrock-{now}"),
            object: "chat.completion".to_owned(),
            created: now,
            model: request.model.clone(),
            choices: vec![Choice {
                index: 0,
                message: ChoiceMessage {
                    role: "assistant".to_owned(),
                    content: content_text,
                    tool_calls,
                },
                finish_reason,
            }],
            usage,
        })
    }

    #[allow(clippy::too_many_lines)]
    async fn complete_stream(
        &self,
        request: &CompletionRequest,
        _context: &RequestContext,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>, LlmError> {
        let (system_blocks, messages) = build_converse_input(request)?;

        let mut converse = self.client.converse_stream().model_id(&request.model);

        for block in &system_blocks {
            converse = converse.system(block.clone());
        }

        for msg in &messages {
            converse = converse.messages(msg.clone());
        }

        converse = converse.inference_config(build_inference_config(request));

        if let Some(tool_config) = build_tool_config(request) {
            converse = converse.tool_config(tool_config);
        }

        let output = converse.send().await.map_err(|e| {
            tracing::error!(provider = %self.name, error = %e, "bedrock converse_stream failed");
            LlmError::Upstream(e.to_string())
        })?;

        let receiver = output.stream;

        // Convert the EventReceiver into a futures::Stream using unfold
        let stream = futures_util::stream::unfold((receiver, 0u32), |(mut receiver, mut tool_call_index)| async move {
            match receiver.recv().await {
                Ok(Some(stream_output)) => {
                    use aws_sdk_bedrockruntime::types::ConverseStreamOutput;

                    let event = match stream_output {
                        ConverseStreamOutput::ContentBlockDelta(delta) => match delta.delta() {
                            Some(aws_sdk_bedrockruntime::types::ContentBlockDelta::Text(text)) => {
                                Some(Ok(StreamEvent::Delta(StreamDelta {
                                    index: 0,
                                    content: Some(text.to_owned()),
                                    tool_call: None,
                                    finish_reason: None,
                                })))
                            }
                            Some(aws_sdk_bedrockruntime::types::ContentBlockDelta::ToolUse(tool)) => {
                                Some(Ok(StreamEvent::Delta(StreamDelta {
                                    index: 0,
                                    content: None,
                                    tool_call: Some(StreamToolCall {
                                        index: tool_call_index,
                                        id: None,
                                        function: Some(StreamFunctionCall {
                                            name: None,
                                            arguments: Some(tool.input().to_owned()),
                                        }),
                                    }),
                                    finish_reason: None,
                                })))
                            }
                            _ => None,
                        },
                        ConverseStreamOutput::ContentBlockStart(start) => match start.start() {
                            Some(aws_sdk_bedrockruntime::types::ContentBlockStart::ToolUse(tool)) => {
                                let current_index = tool_call_index;
                                tool_call_index += 1;
                                Some(Ok(StreamEvent::Delta(StreamDelta {
                                    index: 0,
                                    content: None,
                                    tool_call: Some(StreamToolCall {
                                        index: current_index,
                                        id: Some(tool.tool_use_id().to_owned()),
                                        function: Some(StreamFunctionCall {
                                            name: Some(tool.name().to_owned()),
                                            arguments: None,
                                        }),
                                    }),
                                    finish_reason: None,
                                })))
                            }
                            _ => None,
                        },
                        ConverseStreamOutput::MessageStop(stop) => {
                            let finish_reason = match stop.stop_reason() {
                                aws_sdk_bedrockruntime::types::StopReason::MaxTokens => Some(FinishReason::Length),
                                aws_sdk_bedrockruntime::types::StopReason::ToolUse => Some(FinishReason::ToolCalls),
                                aws_sdk_bedrockruntime::types::StopReason::ContentFiltered => {
                                    Some(FinishReason::ContentFilter)
                                }
                                _ => Some(FinishReason::Stop),
                            };
                            Some(Ok(StreamEvent::Delta(StreamDelta {
                                index: 0,
                                content: None,
                                tool_call: None,
                                finish_reason,
                            })))
                        }
                        ConverseStreamOutput::Metadata(meta) => meta.usage().map(|u| {
                            #[allow(clippy::cast_sign_loss)]
                            Ok(StreamEvent::Usage(Usage {
                                prompt_tokens: u.input_tokens() as u32,
                                completion_tokens: u.output_tokens() as u32,
                                total_tokens: u.total_tokens() as u32,
                            }))
                        }),
                        _ => None,
                    };

                    // If the event is None (unhandled variant), continue to
                    // the next event by returning Some with a skip sentinel.
                    // We use unfold's state to keep receiving.
                    match event {
                        Some(e) => Some((e, (receiver, tool_call_index))),
                        None => {
                            // Skip this event, produce an empty delta to keep the stream going
                            // Actually just recurse by returning a Done event placeholder
                            // We'll filter these out below
                            Some((
                                Ok(StreamEvent::Delta(StreamDelta {
                                    index: 0,
                                    content: None,
                                    tool_call: None,
                                    finish_reason: None,
                                })),
                                (receiver, tool_call_index),
                            ))
                        }
                    }
                }
                Ok(None) => {
                    // Stream ended
                    None
                }
                Err(e) => Some((Err(LlmError::Streaming(e.to_string())), (receiver, tool_call_index))),
            }
        });

        // Filter out empty deltas (skip sentinels)
        let filtered = stream.filter(|event| {
            let keep = match event {
                Ok(StreamEvent::Delta(delta)) => {
                    delta.content.is_some() || delta.tool_call.is_some() || delta.finish_reason.is_some()
                }
                _ => true,
            };
            async move { keep }
        });

        Ok(Box::pin(filtered))
    }
}

/// Build inference configuration from the request params
fn build_inference_config(request: &CompletionRequest) -> InferenceConfiguration {
    let mut config = InferenceConfiguration::builder();

    if let Some(temp) = request.params.temperature {
        #[allow(clippy::cast_possible_truncation)]
        {
            config = config.temperature(temp as f32);
        }
    }
    if let Some(top_p) = request.params.top_p {
        #[allow(clippy::cast_possible_truncation)]
        {
            config = config.top_p(top_p as f32);
        }
    }
    if let Some(max_tokens) = request.params.max_tokens {
        #[allow(clippy::cast_possible_wrap)]
        let max_tokens_i32 = max_tokens as i32;
        config = config.max_tokens(max_tokens_i32);
    }
    if let Some(stop) = &request.params.stop {
        for seq in stop {
            config = config.stop_sequences(seq.clone());
        }
    }

    config.build()
}

/// Build tool configuration from the request
fn build_tool_config(request: &CompletionRequest) -> Option<ToolConfiguration> {
    let tools = request.tools.as_ref()?;

    let tool_specs: Vec<Tool> = tools
        .iter()
        .filter_map(|t| {
            let input_schema = t.function.parameters.as_ref().map_or_else(
                || ToolInputSchema::Json(aws_smithy_types::Document::Object(std::collections::HashMap::new())),
                |p| {
                    let doc = value_to_document(p);
                    ToolInputSchema::Json(doc)
                },
            );

            let mut spec_builder = ToolSpecification::builder()
                .name(&t.function.name)
                .input_schema(input_schema);

            if let Some(desc) = &t.function.description {
                spec_builder = spec_builder.description(desc);
            }

            Some(Tool::ToolSpec(spec_builder.build().ok()?))
        })
        .collect();

    if tool_specs.is_empty() {
        return None;
    }

    let mut tool_config = ToolConfiguration::builder();
    for tool in tool_specs {
        tool_config = tool_config.tools(tool);
    }

    tool_config.build().ok()
}

/// Build Bedrock Converse API input from internal request
fn build_converse_input(
    request: &CompletionRequest,
) -> Result<(Vec<SystemContentBlock>, Vec<BedrockMessage>), LlmError> {
    let mut system_blocks = Vec::new();
    let mut messages = Vec::new();

    for msg in &request.messages {
        match msg.role {
            Role::System => {
                let text = msg.content.as_text();
                system_blocks.push(SystemContentBlock::Text(text));
            }
            Role::User => {
                let content_blocks = build_content_blocks(msg);
                if let Ok(bedrock_msg) = BedrockMessage::builder()
                    .role(ConversationRole::User)
                    .set_content(Some(content_blocks))
                    .build()
                {
                    messages.push(bedrock_msg);
                }
            }
            Role::Assistant => {
                let content_blocks = build_content_blocks(msg);
                if let Ok(bedrock_msg) = BedrockMessage::builder()
                    .role(ConversationRole::Assistant)
                    .set_content(Some(content_blocks))
                    .build()
                {
                    messages.push(bedrock_msg);
                }
            }
            Role::Tool => {
                // Tool results go as user messages with tool result blocks
                let tool_call_id = msg.tool_call_id.clone().unwrap_or_default();
                let result_text = msg.content.as_text();

                let tool_result = ContentBlock::ToolResult(
                    ToolResultBlock::builder()
                        .tool_use_id(tool_call_id)
                        .content(ToolResultContentBlock::Text(result_text))
                        .build()
                        .map_err(|e| LlmError::InvalidRequest(format!("invalid tool result: {e}")))?,
                );

                if let Ok(bedrock_msg) = BedrockMessage::builder()
                    .role(ConversationRole::User)
                    .content(tool_result)
                    .build()
                {
                    messages.push(bedrock_msg);
                }
            }
        }
    }

    Ok((system_blocks, messages))
}

/// Build Bedrock content blocks from an internal message
fn build_content_blocks(msg: &Message) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();

    match &msg.content {
        Content::Text(text) => {
            if !text.is_empty() {
                blocks.push(ContentBlock::Text(text.clone()));
            }
        }
        Content::Parts(parts) => {
            for part in parts {
                match part {
                    crate::types::ContentPart::Text { text } => {
                        blocks.push(ContentBlock::Text(text.clone()));
                    }
                    crate::types::ContentPart::Image { url, .. } => {
                        // Try to parse data URI for inline images
                        if let Some(rest) = url.strip_prefix("data:")
                            && let Some((mime_and_encoding, data)) = rest.split_once(',')
                        {
                            let format = mime_and_encoding.strip_suffix(";base64").unwrap_or(mime_and_encoding);

                            let image_format = match format {
                                "image/png" => aws_sdk_bedrockruntime::types::ImageFormat::Png,
                                "image/gif" => aws_sdk_bedrockruntime::types::ImageFormat::Gif,
                                "image/webp" => aws_sdk_bedrockruntime::types::ImageFormat::Webp,
                                _ => aws_sdk_bedrockruntime::types::ImageFormat::Jpeg,
                            };

                            if let Ok(bytes) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
                            {
                                let image_source = aws_sdk_bedrockruntime::types::ImageSource::Bytes(
                                    aws_smithy_types::Blob::new(bytes),
                                );

                                if let Ok(image_block) = aws_sdk_bedrockruntime::types::ImageBlock::builder()
                                    .format(image_format)
                                    .source(image_source)
                                    .build()
                                {
                                    blocks.push(ContentBlock::Image(image_block));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Add tool calls from assistant messages
    if let Some(tool_calls) = &msg.tool_calls {
        for tc in tool_calls {
            let input = serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                .unwrap_or_else(|_| serde_json::json!({}));

            let doc = value_to_document(&input);
            if let Ok(tool_use) = ToolUseBlock::builder()
                .tool_use_id(&tc.id)
                .name(&tc.function.name)
                .input(doc)
                .build()
            {
                blocks.push(ContentBlock::ToolUse(tool_use));
            }
        }
    }

    // Ensure at least one content block
    if blocks.is_empty() {
        blocks.push(ContentBlock::Text(String::new()));
    }

    blocks
}

/// Extract text content and tool calls from a Bedrock response message
fn extract_bedrock_response(msg: &BedrockMessage) -> (Option<String>, Option<Vec<ToolCall>>) {
    let mut text = String::new();
    let mut tool_calls = Vec::new();

    for block in msg.content() {
        match block {
            ContentBlock::Text(t) => text.push_str(t),
            ContentBlock::ToolUse(tu) => {
                let arguments =
                    serde_json::to_string(&document_to_value(tu.input())).unwrap_or_else(|_| "{}".to_owned());
                tool_calls.push(ToolCall {
                    id: tu.tool_use_id().to_owned(),
                    function: FunctionCall {
                        name: tu.name().to_owned(),
                        arguments,
                    },
                });
            }
            _ => {}
        }
    }

    let content = if text.is_empty() { None } else { Some(text) };
    let calls = if tool_calls.is_empty() { None } else { Some(tool_calls) };

    (content, calls)
}

/// Convert a `serde_json::Value` to an AWS `Document`
fn value_to_document(value: &serde_json::Value) -> aws_smithy_types::Document {
    match value {
        serde_json::Value::Null => aws_smithy_types::Document::Null,
        serde_json::Value::Bool(b) => aws_smithy_types::Document::Bool(*b),
        serde_json::Value::Number(n) =>
        {
            #[allow(clippy::cast_precision_loss)]
            n.as_i64().map_or_else(
                || {
                    n.as_f64().map_or(aws_smithy_types::Document::Null, |f| {
                        aws_smithy_types::Document::Number(aws_smithy_types::Number::Float(f))
                    })
                },
                |i| aws_smithy_types::Document::Number(aws_smithy_types::Number::Float(i as f64)),
            )
        }
        serde_json::Value::String(s) => aws_smithy_types::Document::String(s.clone()),
        serde_json::Value::Array(arr) => aws_smithy_types::Document::Array(arr.iter().map(value_to_document).collect()),
        serde_json::Value::Object(map) => {
            let obj: std::collections::HashMap<String, aws_smithy_types::Document> =
                map.iter().map(|(k, v)| (k.clone(), value_to_document(v))).collect();
            aws_smithy_types::Document::Object(obj)
        }
    }
}

/// Convert an AWS `Document` to a `serde_json::Value`
fn document_to_value(doc: &aws_smithy_types::Document) -> serde_json::Value {
    match doc {
        aws_smithy_types::Document::Object(map) => {
            let obj: serde_json::Map<String, serde_json::Value> =
                map.iter().map(|(k, v)| (k.clone(), document_to_value(v))).collect();
            serde_json::Value::Object(obj)
        }
        aws_smithy_types::Document::Array(arr) => serde_json::Value::Array(arr.iter().map(document_to_value).collect()),
        aws_smithy_types::Document::Number(n) => {
            let f = n.to_f64_lossy();
            serde_json::Number::from_f64(f).map_or(serde_json::Value::Null, serde_json::Value::Number)
        }
        aws_smithy_types::Document::String(s) => serde_json::Value::String(s.clone()),
        aws_smithy_types::Document::Bool(b) => serde_json::Value::Bool(*b),
        aws_smithy_types::Document::Null => serde_json::Value::Null,
    }
}
