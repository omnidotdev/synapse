use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Request to record usage against a meter
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordUsageRequest {
    /// Amount to increment
    pub delta: f64,
    /// Unique key for idempotent recording
    pub idempotency_key: String,
    /// Optional metadata attached to the usage event
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

/// Response from recording usage
///
/// Aether returns `{ billingAccountId, meterId, eventId, ... }` on success,
/// so `accepted` defaults to `true` when deserialization succeeds (HTTP 200)
#[derive(Debug, Clone, Deserialize)]
pub struct RecordUsageResponse {
    /// Whether the recording was accepted
    #[serde(default = "default_true")]
    pub accepted: bool,
}

const fn default_true() -> bool {
    true
}

/// Response from checking usage against limits
#[derive(Debug, Clone, Deserialize)]
pub struct CheckUsageResponse {
    /// Whether the additional usage is within limits
    pub allowed: bool,
    /// Current usage for this meter
    #[serde(default, alias = "current")]
    pub current_usage: f64,
    /// Configured limit for this meter
    #[serde(default)]
    pub limit: Option<f64>,
}

/// Response from checking a feature entitlement
#[derive(Debug, Clone, Deserialize)]
pub struct EntitlementCheckResponse {
    /// Whether access to the feature is granted
    #[serde(alias = "hasEntitlement")]
    pub has_access: bool,
    /// Entitlement version for cache invalidation
    #[serde(default, alias = "version")]
    pub entitlement_version: Option<u64>,
}

/// Single entitlement entry from Aether
#[derive(Debug, Clone, Deserialize)]
pub struct EntitlementEntry {
    /// Feature key
    #[serde(alias = "featureKey")]
    pub feature_key: String,
    /// Entitlement value (number, boolean as string, or JSON)
    pub value: Option<serde_json::Value>,
    /// Source of the entitlement (subscription, manual, etc.)
    #[serde(default)]
    pub source: Option<String>,
}

/// Response listing all entitlements for an entity
#[derive(Debug, Clone, Deserialize)]
pub struct EntitlementsResponse {
    /// List of entitlements
    pub entitlements: Vec<EntitlementEntry>,
    /// Entitlement version for cache invalidation
    #[serde(default, alias = "entitlementVersion")]
    pub entitlement_version: Option<u64>,
}

/// Request to check credit balance sufficiency
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditCheckRequest {
    /// Amount of credits required
    pub amount: f64,
}

/// Response from checking credit balance
#[derive(Debug, Clone, Deserialize)]
pub struct CreditCheckResponse {
    /// Whether sufficient credits are available
    pub sufficient: bool,
    /// Current credit balance
    #[serde(default)]
    pub balance: f64,
}

/// Request to deduct credits
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditDeductRequest {
    /// Amount to deduct
    pub amount: f64,
    /// Description of the charge
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Unique key for idempotent deduction
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Reference type (e.g. "completion")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_type: Option<String>,
    /// Reference identifier
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_id: Option<String>,
}

/// Response from deducting credits
#[derive(Debug, Clone, Deserialize)]
pub struct CreditDeductResponse {
    /// Whether the deduction was successful
    pub success: bool,
    /// Balance after deduction
    #[serde(default, alias = "balance")]
    pub balance_after: f64,
    /// Transaction ID
    #[serde(default, alias = "transactionId")]
    pub transaction_id: Option<String>,
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    // Aether's routes read camelCase bodies and ignore unknown keys, so a
    // snake_case field is silently dropped (the idempotency key included)
    #[test]
    fn deduct_request_uses_aether_camel_case() {
        let body = serde_json::to_value(CreditDeductRequest {
            amount: 1.5,
            description: Some("x".to_owned()),
            idempotency_key: Some("k".to_owned()),
            reference_type: Some("completion".to_owned()),
            reference_id: Some("r".to_owned()),
        })
        .unwrap();

        assert_eq!(body["idempotencyKey"], "k");
        assert_eq!(body["referenceType"], "completion");
        assert_eq!(body["referenceId"], "r");
        assert!(body.get("idempotency_key").is_none());
    }

    #[test]
    fn record_usage_request_uses_aether_camel_case() {
        let body = serde_json::to_value(RecordUsageRequest {
            delta: 1.0,
            idempotency_key: "k".to_owned(),
            metadata: HashMap::new(),
        })
        .unwrap();

        assert_eq!(body["idempotencyKey"], "k");
        assert!(body.get("idempotency_key").is_none());
    }
}
