use http::StatusCode;

/// Trait for domain errors that can be converted to HTTP responses
///
/// Implemented by each feature crate's error type. The server layer
/// converts these into actual HTTP responses, keeping domain errors
/// decoupled from axum.
pub trait HttpError: std::error::Error {
    /// HTTP status code for this error
    fn status_code(&self) -> StatusCode;

    /// Machine-readable error type (e.g. `invalid_request_error`)
    fn error_type(&self) -> &str;

    /// Message safe to expose to API consumers
    fn client_message(&self) -> String;
}

/// Error type of a managed request refused because its account is not a
/// paying customer, distinct from `insufficient_credits_error`
pub const PAYMENT_REQUIRED_ERROR_TYPE: &str = "payment_required";

/// User-safe message for a managed request refused because its account is
/// not a paying customer (a free tier that has never bought credits)
pub const PAYMENT_REQUIRED_MESSAGE: &str =
    "AI generation uses Omni credits. Buy credits or upgrade your plan to start.";
