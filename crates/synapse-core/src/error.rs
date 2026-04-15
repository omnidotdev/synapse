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
