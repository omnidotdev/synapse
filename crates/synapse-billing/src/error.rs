/// Errors returned by the billing client
#[derive(Debug, thiserror::Error)]
pub enum BillingError {
    /// HTTP transport or connection error
    #[error("billing request failed: {0}")]
    Request(#[from] reqwest::Error),

    /// Aether returned a non-success status
    #[error("billing API error ({status}): {message}")]
    Api {
        /// HTTP status from Aether
        status: u16,
        /// Error message from the response body
        message: String,
    },

    /// Billing subsystem is not configured
    #[error("billing is not configured")]
    NotConfigured,

    /// Entitlement check denied access
    #[error("entitlement denied: {feature_key}")]
    EntitlementDenied {
        /// Feature key that was denied
        feature_key: String,
    },

    /// Usage limit exceeded
    #[error("usage limit exceeded for meter: {meter_key}")]
    UsageLimitExceeded {
        /// Meter key that exceeded its limit
        meter_key: String,
    },

    /// Circuit breaker is open due to repeated failures
    #[error("billing service circuit open")]
    CircuitOpen,
}
