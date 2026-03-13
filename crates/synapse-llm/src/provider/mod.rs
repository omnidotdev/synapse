//! Provider trait and implementations for LLM backends

pub mod anthropic;
pub mod bedrock;
pub mod google;
pub mod openai;

use std::pin::Pin;

use async_trait::async_trait;
use futures_util::Stream;
use http::header::{HeaderName, HeaderValue};
use synapse_config::HeaderRuleConfig;
use synapse_core::{
    HeaderForward, HeaderInsert, HeaderRemove, HeaderRenameDuplicate, HeaderRule, NameOrPattern, RequestContext,
    ValidHeaderName, ValidHeaderValue,
};

use crate::error::LlmError;
use crate::types::{CompletionRequest, CompletionResponse, StreamEvent};

/// Capabilities advertised by a provider
#[derive(Debug, Clone)]
pub struct ProviderCapabilities {
    /// Whether the provider supports streaming responses
    pub streaming: bool,
    /// Whether the provider supports tool/function calling
    pub tool_calling: bool,
}

/// Trait implemented by each LLM provider backend
#[async_trait]
pub trait Provider: Send + Sync {
    /// Human-readable provider name
    fn name(&self) -> &str;

    /// Advertised capabilities
    fn capabilities(&self) -> ProviderCapabilities;

    /// Send a non-streaming completion request
    async fn complete(
        &self,
        request: &CompletionRequest,
        context: &RequestContext,
    ) -> Result<CompletionResponse, LlmError>;

    /// Send a streaming completion request
    async fn complete_stream(
        &self,
        request: &CompletionRequest,
        context: &RequestContext,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>, LlmError>;
}

/// Convert config-level header rules to core header rules
///
/// Performs best-effort conversion, logging warnings for invalid entries
pub fn parse_header_rules(configs: &[HeaderRuleConfig]) -> Vec<HeaderRule> {
    configs
        .iter()
        .filter_map(|config| match config {
            HeaderRuleConfig::Forward(fwd) => {
                let name = parse_name_or_pattern(&fwd.name)?;
                let rename = fwd.rename.as_ref().and_then(|r| parse_valid_header_name(r));
                let default = fwd.default.as_ref().and_then(|d| parse_valid_header_value(d));

                Some(HeaderRule::Forward(HeaderForward { name, rename, default }))
            }
            HeaderRuleConfig::Insert(ins) => {
                let name = parse_valid_header_name(&ins.name)?;
                let value = parse_valid_header_value(&ins.value)?;
                Some(HeaderRule::Insert(HeaderInsert { name, value }))
            }
            HeaderRuleConfig::Remove(rem) => {
                let name = parse_name_or_pattern(&rem.name)?;
                Some(HeaderRule::Remove(HeaderRemove { name }))
            }
            HeaderRuleConfig::RenameDuplicate(dup) => {
                let name = parse_valid_header_name(&dup.name)?;
                let rename = parse_valid_header_name(&dup.rename)?;
                let default = dup.default.as_ref().and_then(|d| parse_valid_header_value(d));

                Some(HeaderRule::RenameDuplicate(HeaderRenameDuplicate {
                    name,
                    rename,
                    default,
                }))
            }
        })
        .collect()
}

/// Parse a string into a `NameOrPattern`
fn parse_name_or_pattern(s: &str) -> Option<NameOrPattern> {
    // If the string contains regex metacharacters, treat as pattern
    if s.contains('*') || s.contains('?') || s.contains('[') || s.contains('(') {
        let regex = regex::Regex::new(s).ok()?;
        Some(NameOrPattern::Pattern(synapse_core::HeaderPattern(regex)))
    } else {
        let name = HeaderName::try_from(s).ok()?;
        Some(NameOrPattern::Name(ValidHeaderName::new(name)))
    }
}

/// Parse a string into a `ValidHeaderName`
fn parse_valid_header_name(s: &str) -> Option<ValidHeaderName> {
    HeaderName::try_from(s).ok().map(ValidHeaderName::new)
}

/// Parse a string into a `ValidHeaderValue`
fn parse_valid_header_value(s: &str) -> Option<ValidHeaderValue> {
    HeaderValue::try_from(s).ok().map(ValidHeaderValue::new)
}
