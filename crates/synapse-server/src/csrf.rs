use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::StatusCode;

/// CSRF protection middleware
///
/// Checks that non-GET/HEAD/OPTIONS requests include the configured
/// CSRF header. The header value is ignored — its presence is enough to
/// prove the request was made from JavaScript (not a plain form submit).
pub async fn csrf_middleware(header_name: String, request: Request, next: Next) -> Response {
    let method = request.method().clone();

    // Safe methods don't need CSRF protection
    if method == http::Method::GET || method == http::Method::HEAD || method == http::Method::OPTIONS {
        return next.run(request).await;
    }

    // Check for CSRF header
    let header = http::header::HeaderName::try_from(&header_name);
    match header {
        Ok(name) => {
            if request.headers().contains_key(&name) {
                next.run(request).await
            } else {
                (StatusCode::FORBIDDEN, format!("missing CSRF header: {header_name}")).into_response()
            }
        }
        Err(_) => next.run(request).await,
    }
}
