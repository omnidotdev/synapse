use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use synapse_ratelimit::RequestLimiter;

/// Rate limiting middleware using an Arc-wrapped limiter
pub async fn rate_limit_middleware_arc(limiter: Arc<RequestLimiter>, request: Request, next: Next) -> Response {
    // Check global rate limit
    if let Err(e) = limiter.check_global().await {
        return rate_limit_response(&e);
    }

    // Check per-IP rate limit
    if let Some(ip) = extract_client_ip(&request)
        && let Err(e) = limiter.check_ip(&ip).await
    {
        return rate_limit_response(&e);
    }

    next.run(request).await
}

fn extract_client_ip(request: &Request) -> Option<String> {
    // Try X-Forwarded-For first
    if let Some(forwarded) = request.headers().get("x-forwarded-for")
        && let Ok(val) = forwarded.to_str()
        && let Some(first) = val.split(',').next()
    {
        return Some(first.trim().to_string());
    }

    // Try X-Real-IP
    if let Some(real_ip) = request.headers().get("x-real-ip")
        && let Ok(val) = real_ip.to_str()
    {
        return Some(val.trim().to_string());
    }

    None
}

fn rate_limit_response(error: &synapse_ratelimit::RateLimitError) -> Response {
    match error {
        synapse_ratelimit::RateLimitError::Exceeded { retry_after } => {
            let body = serde_json::json!({
                "error": {
                    "type": "rate_limited",
                    "message": format!("rate limit exceeded, retry after {retry_after}s"),
                }
            });

            let mut response = (StatusCode::TOO_MANY_REQUESTS, axum::Json(body)).into_response();

            if let Ok(val) = retry_after.to_string().parse() {
                response.headers_mut().insert("retry-after", val);
            }

            response
        }
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "rate limiter error").into_response(),
    }
}
