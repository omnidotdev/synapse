//! Guardrails middleware for content filtering on LLM requests

use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use synapse_core::TokenLimits;
use synapse_guardrails::GuardrailEngine;

/// Check request body content against guardrails
///
/// When a `TokenLimits` extension is present (set by the entitlement
/// middleware), the per-request input token limit from the user's tier
/// overrides the static `MaxInputTokens` guardrail rule. This ensures
/// free-tier users are capped at 8,192 tokens while Pro/Team retain
/// 200,000.
pub async fn guardrails_middleware(engine: Arc<GuardrailEngine>, request: Request, next: Next) -> Response {
    // Only check LLM completion endpoints
    let path = request.uri().path();
    let is_llm_endpoint = path == "/v1/chat/completions" || path == "/v1/messages";

    if !is_llm_endpoint {
        return next.run(request).await;
    }

    // Extract tier-specific token limits if set by entitlement middleware
    let token_limits = request.extensions().get::<TokenLimits>().cloned();

    // Buffer the request body for inspection
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 10 * 1024 * 1024).await else {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            axum::Json(serde_json::json!({
                "error": {
                    "message": "request body too large for guardrail inspection",
                    "type": "invalid_request_error"
                }
            })),
        )
            .into_response();
    };

    // Extract text content from the request for inspection
    if let Ok(body_str) = std::str::from_utf8(&bytes) {
        let content = extract_message_content(body_str);
        if !content.is_empty() {
            // Enforce tier-specific input token limit before the generic guardrails.
            // The entitlement middleware resolves the per-request cap from Aether
            // (free=8192, pro/team=200000); this check runs it against the
            // estimated token count
            if let Some(ref limits) = token_limits {
                // Use char-based estimation (~4 chars per token) which handles
                // code and whitespace-sparse content much better than word-based
                let estimated_tokens = content.len().div_ceil(4);

                if estimated_tokens > limits.max_input_tokens {
                    tracing::warn!(
                        estimated_tokens,
                        limit = limits.max_input_tokens,
                        "per-request input token limit exceeded"
                    );
                    return (
                        StatusCode::FORBIDDEN,
                        axum::Json(serde_json::json!({
                            "error": {
                                "message": format!(
                                    "request blocked by content policy: estimated {estimated_tokens} input tokens exceeds your plan limit of {}",
                                    limits.max_input_tokens
                                ),
                                "type": "content_policy_violation",
                                "code": "input_token_limit_exceeded"
                            }
                        })),
                    )
                        .into_response();
                }
            }

            // Run the static guardrail rules (PII, keywords, absolute token ceiling)
            let result = engine.check(&content);
            if result.blocked {
                let (rule, reason) = result
                    .block_reason
                    .unwrap_or_else(|| ("unknown".to_owned(), "content blocked".to_owned()));
                tracing::warn!(rule, reason, "guardrails blocked request");

                return (
                    StatusCode::FORBIDDEN,
                    axum::Json(serde_json::json!({
                        "error": {
                            "message": format!("request blocked by content policy: {reason}"),
                            "type": "content_policy_violation",
                            "code": "content_blocked"
                        }
                    })),
                )
                    .into_response();
            }
        }
    }

    // Also enforce per-request max_output_tokens by clamping the request body.
    // If the user requested more output tokens than their plan allows, cap it
    let body_bytes: Vec<u8> = token_limits.as_ref().map_or_else(
        || bytes.to_vec(),
        |limits| clamp_max_tokens(&bytes, limits.max_output_tokens),
    );

    // Reconstruct request with buffered body
    let request = Request::from_parts(parts, Body::from(body_bytes));
    next.run(request).await
}

/// Clamp the `max_tokens` field in the request body to the tier limit
///
/// If the requested `max_tokens` exceeds the plan limit, silently cap it.
/// This is friendlier than rejecting the request outright for output tokens
fn clamp_max_tokens(bytes: &[u8], limit: usize) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return bytes.to_vec();
    };

    let mut modified = false;

    // OpenAI format: `max_tokens` or `max_completion_tokens`
    // Anthropic format: `max_tokens` (same field, handled here)
    for field in &["max_tokens", "max_completion_tokens"] {
        if let Some(current) = value.get(*field).and_then(serde_json::Value::as_u64)
            && current > limit as u64
        {
            value[*field] = serde_json::Value::Number(serde_json::Number::from(limit));
            modified = true;
            tracing::debug!(
                field,
                requested = current,
                clamped = limit,
                "clamped output token limit"
            );
        }
    }

    if modified {
        serde_json::to_vec(&value).unwrap_or_else(|_| bytes.to_vec())
    } else {
        bytes.to_vec()
    }
}

/// Extract message text content from a JSON request body
fn extract_message_content(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return String::new();
    };

    let mut content_parts = Vec::new();

    // Extract from messages array (OpenAI and Anthropic formats)
    if let Some(messages) = value.get("messages").and_then(|m| m.as_array()) {
        for msg in messages {
            // String content
            if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
                content_parts.push(text.to_owned());
            }
            // Array content (multipart)
            if let Some(parts) = msg.get("content").and_then(|c| c.as_array()) {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        content_parts.push(text.to_owned());
                    }
                }
            }
        }
    }

    content_parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_openai_messages() {
        let body = r#"{"model": "gpt-4o", "messages": [{"role": "user", "content": "Hello world"}]}"#;
        let content = extract_message_content(body);
        assert_eq!(content, "Hello world");
    }

    #[test]
    fn extract_multipart_messages() {
        let body = r#"{"model": "gpt-4o", "messages": [{"role": "user", "content": [{"type": "text", "text": "Look at this"}, {"type": "image", "url": "..."}]}]}"#;
        let content = extract_message_content(body);
        assert_eq!(content, "Look at this");
    }

    #[test]
    fn extract_multiple_messages() {
        let body = r#"{"messages": [{"role": "system", "content": "You are helpful"}, {"role": "user", "content": "Hi there"}]}"#;
        let content = extract_message_content(body);
        assert_eq!(content, "You are helpful\nHi there");
    }

    #[test]
    fn extract_empty_on_invalid_json() {
        let content = extract_message_content("not json");
        assert!(content.is_empty());
    }

    #[test]
    fn clamp_max_tokens_caps_over_limit() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 50000
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let result = clamp_max_tokens(&bytes, 4096);
        let parsed: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(parsed["max_tokens"], 4096);
    }

    #[test]
    fn clamp_max_tokens_preserves_under_limit() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 1000
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let result = clamp_max_tokens(&bytes, 4096);
        let parsed: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(parsed["max_tokens"], 1000);
    }

    #[test]
    fn clamp_max_completion_tokens_caps_over_limit() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "max_completion_tokens": 100_000
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let result = clamp_max_tokens(&bytes, 32768);
        let parsed: serde_json::Value = serde_json::from_slice(&result).unwrap();
        assert_eq!(parsed["max_completion_tokens"], 32768);
    }

    #[test]
    fn clamp_max_tokens_handles_invalid_json() {
        let bytes = b"not json";
        let result = clamp_max_tokens(bytes, 4096);
        assert_eq!(result, bytes);
    }

    #[test]
    fn char_based_token_estimation_handles_code() {
        // Code with minimal whitespace: "fn main(){let x=vec![1,2,3];}" = 30 chars
        let code = "fn main(){let x=vec![1,2,3];}";
        let estimated = code.len().div_ceil(4);
        // 30 chars / 4 = ~8 tokens (reasonable)
        assert_eq!(estimated, 8);

        // Word-based would give: 3 words * 1.3 = 4 tokens (undercount for dense code)
        let word_count = code.split_whitespace().count();
        assert_eq!(word_count, 3); // proves word-based undercounts for code
    }

    #[test]
    fn char_based_token_estimation_handles_prose() {
        // Normal English prose: ~5 chars per word, ~1.3 tokens per word
        // "Hello world this is a test" = 26 chars, 6 words
        let prose = "Hello world this is a test";
        let estimated = prose.len().div_ceil(4);
        // 26 chars / 4 = ~7 tokens (close to 6 words * 1.3 = 7.8)
        assert!((6..=9).contains(&estimated));
    }
}
