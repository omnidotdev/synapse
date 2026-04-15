//! Internal canonical types for LLM request/response representation
//!
//! These types are provider-agnostic and serve as the normalized internal
//! representation that all wire formats convert to and from.

pub mod message;
pub mod request;
pub mod response;
pub mod stream;
pub mod tool;

pub use message::{Content, ContentPart, FunctionCall, Message, Role, ToolCall, ToolResult};
pub use request::{CompletionParams, CompletionRequest};
pub use response::{Choice, ChoiceMessage, CompletionResponse, FinishReason, Usage};
pub use stream::{StreamDelta, StreamEvent, StreamFunctionCall, StreamToolCall};
pub use tool::{
    FunctionDefinition, ToolChoice, ToolChoiceFunction, ToolChoiceFunctionName, ToolChoiceMode, ToolDefinition,
};
