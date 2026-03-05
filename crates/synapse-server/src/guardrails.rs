//! Guardrails middleware for content filtering on LLM requests

use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use synapse_guardrails::GuardrailEngine;

/// Check request body content against guardrails
pub async fn guardrails_middleware(engine: Arc<GuardrailEngine>, request: Request, next: Next) -> Response {
    // Only check LLM completion endpoints
    let path = request.uri().path();
    let is_llm_endpoint = path == "/v1/chat/completions" || path == "/v1/messages";

    if !is_llm_endpoint {
        return next.run(request).await;
    }

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

    // Reconstruct request with buffered body
    let request = Request::from_parts(parts, Body::from(bytes));
    next.run(request).await
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
}
