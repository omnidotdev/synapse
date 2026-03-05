//! Heuristic query analysis for smart routing
//!
//! Classifies queries by task type and complexity using token counting
//! and pattern matching. No ML pipeline — pure heuristics.

use std::sync::LazyLock;

use regex::Regex;
use tiktoken_rs::o200k_base;

/// Token threshold above which a request is considered long-context
const LONG_CONTEXT_THRESHOLD: usize = 30_000;

/// Broad task classification for a query
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskType {
    /// Code generation, debugging, implementation
    Code,
    /// Mathematical reasoning, calculations, proofs
    Math,
    /// Creative writing, storytelling
    Creative,
    /// Simple factual questions
    SimpleQa,
    /// Data analysis, analytics, statistical queries
    Analysis,
    /// General queries that don't fit other categories
    General,
}

/// Complexity level for routing decisions
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Complexity {
    Low,
    Medium,
    High,
}

/// Capabilities the request requires from a model
#[derive(Debug, Clone, Default)]
pub struct RequiredCapabilities {
    /// Request includes tool definitions
    pub tool_calling: bool,
    /// Messages contain image content parts
    pub vision: bool,
    /// Estimated input tokens exceed long-context threshold
    pub long_context: bool,
}

/// Profile of a query for routing decisions
#[derive(Debug, Clone)]
pub struct QueryProfile {
    /// Estimated input token count
    pub estimated_input_tokens: usize,
    /// Classified task type
    pub task_type: TaskType,
    /// Overall complexity assessment
    pub complexity: Complexity,
    /// Whether the request uses tool calling
    pub requires_tool_use: bool,
    /// Capabilities the request requires
    pub required_capabilities: RequiredCapabilities,
    /// Number of messages in the conversation
    pub message_count: usize,
    /// Whether a system prompt is present
    pub has_system_prompt: bool,
}

/// Structured input for query analysis
#[allow(clippy::struct_excessive_bools)]
pub struct AnalysisInput<'a> {
    /// Raw message JSON values
    pub messages: &'a [serde_json::Value],
    /// Whether tool definitions are present
    pub has_tools: bool,
    /// Whether any message contains image content
    pub has_images: bool,
    /// Total message count
    pub message_count: usize,
    /// Whether a system prompt is present
    pub has_system_prompt: bool,
    /// Number of assistant turns with tool calls
    pub tool_call_turns: usize,
    /// Whether this is a multi-turn conversation (> 1 user message)
    pub is_multi_turn: bool,
}

/// Analyze a query for routing decisions using structured input
pub fn analyze_query_structured(input: &AnalysisInput) -> QueryProfile {
    let full_text = extract_text(input.messages);
    let last_user = extract_last_user_message(input.messages);
    let estimated_input_tokens = estimate_tokens(&full_text);
    let task_type = classify_task(&last_user);

    // Long conversations (> 10 messages) also suggest long-context preference
    let needs_long_context = estimated_input_tokens > LONG_CONTEXT_THRESHOLD || input.message_count > 10;

    let required_capabilities = RequiredCapabilities {
        tool_calling: input.has_tools || input.tool_call_turns > 0,
        vision: input.has_images,
        long_context: needs_long_context,
    };

    let complexity = assess_complexity(estimated_input_tokens, task_type, input);

    QueryProfile {
        estimated_input_tokens,
        task_type,
        complexity,
        requires_tool_use: input.has_tools,
        required_capabilities,
        message_count: input.message_count,
        has_system_prompt: input.has_system_prompt,
    }
}

/// Analyze a query for routing decisions
///
/// Backward-compatible wrapper around `analyze_query_structured`
pub fn analyze_query(messages: &[serde_json::Value], has_tools: bool) -> QueryProfile {
    let message_count = messages.len();
    let has_system_prompt = messages
        .iter()
        .any(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"));

    let input = AnalysisInput {
        messages,
        has_tools,
        has_images: false,
        message_count,
        has_system_prompt,
        tool_call_turns: 0,
        is_multi_turn: false,
    };

    analyze_query_structured(&input)
}

/// Extract all text content from messages for token counting
fn extract_text(messages: &[serde_json::Value]) -> String {
    let mut text = String::new();
    for msg in messages {
        if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
            text.push_str(content);
            text.push('\n');
        }
    }
    text
}

/// Extract the last user message content
fn extract_last_user_message(messages: &[serde_json::Value]) -> String {
    messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
        .unwrap_or("")
        .to_owned()
}

/// Estimate token count using tiktoken
fn estimate_tokens(text: &str) -> usize {
    o200k_base().map_or_else(|_| text.len() / 4, |bpe| bpe.encode_with_special_tokens(text).len())
}

// -- Regex patterns compiled once via LazyLock --

static CODE_FENCE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"```\w*\n").unwrap());

static FILE_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[\w./\\-]+\.(rs|ts|tsx|js|jsx|py|go|java|cpp|c|h|rb|php|swift|kt)\b").unwrap());

static IMPORT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^(?:use |import |from |require\(|#include )").unwrap());

static FUNC_SIG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:fn |def |func |function |pub fn |async fn |const |let |var )\w+\s*[\(<{]").unwrap()
});

static LATEX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\(?:frac|sum|int|prod|lim|sqrt|begin\{equation\})").unwrap());

static EQUATION_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[=<>≤≥≠±×÷∈∉⊂⊃∀∃]").unwrap());

static ANALYSIS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:analyze|analyse|correlat|regression|distribution|dataset|csv|dataframe|pivot|aggregate|trend|outlier|histogram|scatter\s?plot)\b")
        .unwrap()
});

/// Classify the task type from the last user message
fn classify_task(text: &str) -> TaskType {
    let lower = text.to_lowercase();

    if is_code_task(&lower, text) {
        TaskType::Code
    } else if is_math_task(&lower, text) {
        TaskType::Math
    } else if is_analysis_task(&lower) {
        TaskType::Analysis
    } else if is_creative_task(&lower) {
        TaskType::Creative
    } else if is_simple_qa(&lower, text) {
        TaskType::SimpleQa
    } else {
        TaskType::General
    }
}

fn is_code_task(lower: &str, original: &str) -> bool {
    // Code fences with language identifier
    if CODE_FENCE_RE.is_match(original) || lower.contains("```") {
        return true;
    }

    // File paths with extensions
    if FILE_PATH_RE.is_match(original) {
        return true;
    }

    // Import/use statements
    if IMPORT_RE.is_match(original) {
        return true;
    }

    // Function signatures
    if FUNC_SIG_RE.is_match(original) {
        return true;
    }

    // Programming keywords
    let code_keywords = [
        "implement",
        "debug",
        "function",
        "refactor",
        "compile",
        "runtime error",
        "syntax error",
        "code review",
        "write a program",
        "write code",
        "fix this code",
        "bug in",
        "stack trace",
        "unit test",
    ];
    code_keywords.iter().any(|k| lower.contains(k))
}

fn is_math_task(lower: &str, original: &str) -> bool {
    // LaTeX patterns
    if LATEX_RE.is_match(original) {
        return true;
    }

    // Mathematical operators and symbols
    let symbol_count = EQUATION_RE.find_iter(original).count();
    if symbol_count >= 3 {
        return true;
    }

    let math_keywords = [
        "calculate",
        "solve",
        "prove",
        "equation",
        "integral",
        "derivative",
        "theorem",
        "∫",
        "∑",
        "∏",
        "matrix",
        "eigenvalue",
        "probability",
    ];
    math_keywords.iter().any(|k| lower.contains(k))
}

fn is_analysis_task(lower: &str) -> bool {
    ANALYSIS_RE.is_match(lower)
}

fn is_creative_task(lower: &str) -> bool {
    let creative_keywords = [
        "write a story",
        "write a poem",
        "creative writing",
        "compose",
        "fictional",
        "narrative",
        "write me a",
        "tell me a story",
    ];
    creative_keywords.iter().any(|k| lower.contains(k))
}

fn is_simple_qa(lower: &str, original: &str) -> bool {
    // Short single-turn questions
    if original.len() > 200 {
        return false;
    }

    let qa_prefixes = [
        "what is",
        "what are",
        "who is",
        "who was",
        "when did",
        "where is",
        "how many",
        "how much",
        "define ",
        "what does",
    ];
    qa_prefixes.iter().any(|p| lower.starts_with(p))
}

/// Assess overall complexity from multiple signals
fn assess_complexity(tokens: usize, task_type: TaskType, input: &AnalysisInput) -> Complexity {
    // Tool use always bumps complexity
    if input.has_tools {
        return Complexity::High;
    }

    let mut base = match task_type {
        TaskType::SimpleQa => Complexity::Low,
        TaskType::Code | TaskType::Math => {
            if tokens > 2000 {
                Complexity::High
            } else {
                Complexity::Medium
            }
        }
        TaskType::Analysis => {
            if tokens > 1500 {
                Complexity::High
            } else {
                Complexity::Medium
            }
        }
        TaskType::Creative => {
            if tokens > 1000 {
                Complexity::High
            } else {
                Complexity::Medium
            }
        }
        TaskType::General => {
            if tokens > 3000 {
                Complexity::High
            } else if tokens > 500 {
                Complexity::Medium
            } else {
                Complexity::Low
            }
        }
    };

    // Multi-turn conversations with tool calls bump complexity
    if input.is_multi_turn && input.tool_call_turns > 0 && base < Complexity::High {
        base = bump_complexity(base);
    }

    // Long conversations (> 10 messages) bump complexity
    if input.message_count > 10 && base < Complexity::High {
        base = bump_complexity(base);
    }

    // System prompt + image adds complexity
    if input.has_system_prompt && input.has_images && base < Complexity::High {
        base = bump_complexity(base);
    }

    base
}

/// Bump complexity by one level
const fn bump_complexity(c: Complexity) -> Complexity {
    match c {
        Complexity::Low => Complexity::Medium,
        Complexity::Medium | Complexity::High => Complexity::High,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_msg(content: &str) -> serde_json::Value {
        serde_json::json!({"role": "user", "content": content})
    }

    #[test]
    fn code_detection() {
        assert_eq!(classify_task("implement a function to sort"), TaskType::Code);
        assert_eq!(classify_task("fix this code ```rust\nfn main()```"), TaskType::Code);
        assert_eq!(classify_task("debug the error in main.rs"), TaskType::Code);
    }

    #[test]
    fn code_detection_file_paths() {
        assert_eq!(classify_task("look at src/lib.rs for the issue"), TaskType::Code);
        assert_eq!(classify_task("the bug is in components/App.tsx"), TaskType::Code);
    }

    #[test]
    fn code_detection_imports() {
        assert_eq!(classify_task("use std::collections::HashMap;"), TaskType::Code);
        assert_eq!(classify_task("import React from 'react'"), TaskType::Code);
    }

    #[test]
    fn code_detection_func_signatures() {
        assert_eq!(classify_task("fn main() {"), TaskType::Code);
        assert_eq!(classify_task("def process_data(input):"), TaskType::Code);
    }

    #[test]
    fn math_detection() {
        assert_eq!(classify_task("solve this equation: 2x + 3 = 7"), TaskType::Math);
        assert_eq!(classify_task("calculate the integral"), TaskType::Math);
    }

    #[test]
    fn math_detection_latex() {
        assert_eq!(
            classify_task("simplify \\frac{a}{b} + \\sum_{i=1}^n x_i"),
            TaskType::Math
        );
    }

    #[test]
    fn math_detection_symbols() {
        assert_eq!(classify_task("if x ≤ y and y ≥ z then x ≠ z"), TaskType::Math);
    }

    #[test]
    fn analysis_detection() {
        assert_eq!(classify_task("analyze the sales data for Q4"), TaskType::Analysis);
        assert_eq!(classify_task("run a regression on the dataset"), TaskType::Analysis);
        assert_eq!(
            classify_task("show me the distribution of user ages"),
            TaskType::Analysis
        );
    }

    #[test]
    fn creative_detection() {
        assert_eq!(classify_task("write a story about a dragon"), TaskType::Creative);
        assert_eq!(classify_task("write a poem about the sea"), TaskType::Creative);
    }

    #[test]
    fn simple_qa_detection() {
        assert_eq!(classify_task("what is rust?"), TaskType::SimpleQa);
        assert_eq!(classify_task("who is Ada Lovelace?"), TaskType::SimpleQa);
    }

    #[test]
    fn general_detection() {
        assert_eq!(classify_task("explain how DNS works in detail"), TaskType::General);
    }

    #[test]
    fn complexity_tool_use_is_high() {
        let profile = analyze_query(&[user_msg("hello")], true);
        assert_eq!(profile.complexity, Complexity::High);
    }

    #[test]
    fn complexity_simple_qa_is_low() {
        let profile = analyze_query(&[user_msg("what is rust?")], false);
        assert_eq!(profile.complexity, Complexity::Low);
    }

    #[test]
    fn required_capabilities_tools() {
        let input = AnalysisInput {
            messages: &[user_msg("help me")],
            has_tools: true,
            has_images: false,
            message_count: 1,
            has_system_prompt: false,
            tool_call_turns: 0,
            is_multi_turn: false,
        };
        let profile = analyze_query_structured(&input);
        assert!(profile.required_capabilities.tool_calling);
        assert!(!profile.required_capabilities.vision);
    }

    #[test]
    fn required_capabilities_images() {
        let input = AnalysisInput {
            messages: &[user_msg("describe this image")],
            has_tools: false,
            has_images: true,
            message_count: 1,
            has_system_prompt: false,
            tool_call_turns: 0,
            is_multi_turn: false,
        };
        let profile = analyze_query_structured(&input);
        assert!(profile.required_capabilities.vision);
    }

    #[test]
    fn required_capabilities_prior_tool_calls() {
        // Even without current tools, prior tool call turns require tool_calling
        let input = AnalysisInput {
            messages: &[user_msg("what was the result?")],
            has_tools: false,
            has_images: false,
            message_count: 5,
            has_system_prompt: false,
            tool_call_turns: 2,
            is_multi_turn: true,
        };
        let profile = analyze_query_structured(&input);
        assert!(profile.required_capabilities.tool_calling);
    }

    #[test]
    fn multi_turn_tool_calls_bump_complexity() {
        let input = AnalysisInput {
            messages: &[user_msg("what is rust?")],
            has_tools: false,
            has_images: false,
            message_count: 5,
            has_system_prompt: false,
            tool_call_turns: 1,
            is_multi_turn: true,
        };
        let profile = analyze_query_structured(&input);
        // SimpleQa starts at Low, bumped to Medium by multi-turn + tool calls
        assert_eq!(profile.complexity, Complexity::Medium);
    }

    #[test]
    fn long_conversation_bumps_complexity() {
        let input = AnalysisInput {
            messages: &[user_msg("what is rust?")],
            has_tools: false,
            has_images: false,
            message_count: 15,
            has_system_prompt: false,
            tool_call_turns: 0,
            is_multi_turn: false,
        };
        let profile = analyze_query_structured(&input);
        // SimpleQa starts at Low, bumped to Medium by message count
        assert_eq!(profile.complexity, Complexity::Medium);
    }

    #[test]
    fn long_conversation_sets_long_context() {
        let input = AnalysisInput {
            messages: &[user_msg("continue the discussion")],
            has_tools: false,
            has_images: false,
            message_count: 15,
            has_system_prompt: false,
            tool_call_turns: 0,
            is_multi_turn: true,
        };
        let profile = analyze_query_structured(&input);
        assert!(profile.required_capabilities.long_context);
    }

    #[test]
    fn prior_tool_calls_affect_capabilities() {
        let input = AnalysisInput {
            messages: &[user_msg("what happened?")],
            has_tools: false,
            has_images: false,
            message_count: 6,
            has_system_prompt: false,
            tool_call_turns: 3,
            is_multi_turn: true,
        };
        let profile = analyze_query_structured(&input);
        // Prior tool calls should require tool_calling capability
        assert!(profile.required_capabilities.tool_calling);
        // Multi-turn + tool calls should bump complexity from Low
        assert!(profile.complexity >= Complexity::Medium);
    }

    #[test]
    fn enriched_profile_fields() {
        let msgs = vec![
            serde_json::json!({"role": "system", "content": "You are helpful"}),
            user_msg("hello"),
        ];
        let profile = analyze_query(&msgs, false);
        assert_eq!(profile.message_count, 2);
        assert!(profile.has_system_prompt);
    }
}
