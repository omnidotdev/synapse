use std::collections::HashMap;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use url::Url;

use crate::circuit::CircuitBreaker;
use crate::error::BillingError;
use crate::types::{
    CheckUsageResponse, CreditCheckResponse, CreditDeductRequest, CreditDeductResponse, EntitlementCheckResponse,
    EntitlementsResponse, RecordUsageRequest, RecordUsageResponse,
};

/// Async HTTP client for the Aether billing API
#[derive(Clone)]
pub struct AetherClient {
    http: reqwest::Client,
    base_url: Url,
    app_id: String,
    service_api_key: SecretString,
    circuit: CircuitBreaker,
}

impl AetherClient {
    /// Create a new Aether client
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be built
    pub fn new(base_url: Url, app_id: String, service_api_key: SecretString) -> Result<Self, BillingError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(BillingError::Request)?;

        Ok(Self {
            http,
            base_url,
            app_id,
            service_api_key,
            circuit: CircuitBreaker::new(),
        })
    }

    /// Record usage against a meter
    ///
    /// POST `/usage/:appId/:entityType/:entityId/:meterKey/record`
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails or Aether returns an error
    pub async fn record_usage(
        &self,
        entity_type: &str,
        entity_id: &str,
        meter_key: &str,
        delta: f64,
        idempotency_key: &str,
        metadata: HashMap<String, String>,
    ) -> Result<RecordUsageResponse, BillingError> {
        let url = self
            .base_url
            .join(&format!(
                "usage/{}/{entity_type}/{entity_id}/{meter_key}/record",
                self.app_id
            ))
            .map_err(|e| BillingError::Api {
                status: 0,
                message: format!("invalid URL: {e}"),
            })?;

        self.circuit.check()?;

        let body = RecordUsageRequest {
            delta,
            idempotency_key: idempotency_key.to_owned(),
            metadata,
        };

        let result = self
            .http
            .post(url)
            .header("x-service-api-key", self.service_api_key.expose_secret())
            .json(&body)
            .send()
            .await;

        match result {
            Ok(response) if response.status().is_success() => {
                self.circuit.record_success();
                Ok(response.json().await?)
            }
            Ok(response) => {
                self.circuit.record_failure();
                let status = response.status().as_u16();
                let message = response.text().await.unwrap_or_default();
                Err(BillingError::Api { status, message })
            }
            Err(e) => {
                self.circuit.record_failure();
                Err(BillingError::Request(e))
            }
        }
    }

    /// Check whether additional usage is within limits
    ///
    /// GET `/usage/:appId/:entityType/:entityId/:meterKey/check`
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails or Aether returns an error
    pub async fn check_usage(
        &self,
        entity_type: &str,
        entity_id: &str,
        meter_key: &str,
        additional_usage: f64,
    ) -> Result<CheckUsageResponse, BillingError> {
        let url = self
            .base_url
            .join(&format!(
                "usage/{}/{entity_type}/{entity_id}/{meter_key}/check",
                self.app_id
            ))
            .map_err(|e| BillingError::Api {
                status: 0,
                message: format!("invalid URL: {e}"),
            })?;

        self.circuit.check()?;

        let result = self
            .http
            .get(url)
            .header("x-service-api-key", self.service_api_key.expose_secret())
            .query(&[("additionalUsage", additional_usage.to_string())])
            .send()
            .await;

        match result {
            Ok(response) if response.status().is_success() => {
                self.circuit.record_success();
                Ok(response.json().await?)
            }
            Ok(response) => {
                self.circuit.record_failure();
                let status = response.status().as_u16();
                let message = response.text().await.unwrap_or_default();
                Err(BillingError::Api { status, message })
            }
            Err(e) => {
                self.circuit.record_failure();
                Err(BillingError::Request(e))
            }
        }
    }

    /// Check whether an entity has access to a feature
    ///
    /// GET `/entitlements/:appId/:entityType/:entityId/:featureKey/check`
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails or Aether returns an error
    pub async fn check_entitlement(
        &self,
        entity_type: &str,
        entity_id: &str,
        feature_key: &str,
    ) -> Result<EntitlementCheckResponse, BillingError> {
        let url = self
            .base_url
            .join(&format!(
                "entitlements/{}/{entity_type}/{entity_id}/{feature_key}/check",
                self.app_id
            ))
            .map_err(|e| BillingError::Api {
                status: 0,
                message: format!("invalid URL: {e}"),
            })?;

        self.circuit.check()?;

        let result = self
            .http
            .get(url)
            .header("x-service-api-key", self.service_api_key.expose_secret())
            .send()
            .await;

        match result {
            Ok(response) if response.status().is_success() => {
                self.circuit.record_success();
                Ok(response.json().await?)
            }
            Ok(response) => {
                self.circuit.record_failure();
                let status = response.status().as_u16();
                let message = response.text().await.unwrap_or_default();
                Err(BillingError::Api { status, message })
            }
            Err(e) => {
                self.circuit.record_failure();
                Err(BillingError::Request(e))
            }
        }
    }

    /// Retrieve all entitlements for an entity
    ///
    /// GET `/entitlements/:appId/:entityType/:entityId`
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails or Aether returns an error
    pub async fn get_entitlements(
        &self,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<EntitlementsResponse, BillingError> {
        let url = self
            .base_url
            .join(&format!("entitlements/{}/{entity_type}/{entity_id}", self.app_id))
            .map_err(|e| BillingError::Api {
                status: 0,
                message: format!("invalid URL: {e}"),
            })?;

        self.circuit.check()?;

        let result = self
            .http
            .get(url)
            .header("x-service-api-key", self.service_api_key.expose_secret())
            .send()
            .await;

        match result {
            Ok(response) if response.status().is_success() => {
                self.circuit.record_success();
                Ok(response.json().await?)
            }
            Ok(response) => {
                self.circuit.record_failure();
                let status = response.status().as_u16();
                let message = response.text().await.unwrap_or_default();
                Err(BillingError::Api { status, message })
            }
            Err(e) => {
                self.circuit.record_failure();
                Err(BillingError::Request(e))
            }
        }
    }

    /// Check whether an entity has sufficient credits
    ///
    /// GET `/credits/:appId/:entityType/:entityId/check?amount=:amount`
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails or Aether returns an error
    pub async fn check_credits(
        &self,
        entity_type: &str,
        entity_id: &str,
        amount: f64,
    ) -> Result<CreditCheckResponse, BillingError> {
        let url = self
            .base_url
            .join(&format!("credits/{}/{entity_type}/{entity_id}/check", self.app_id))
            .map_err(|e| BillingError::Api {
                status: 0,
                message: format!("invalid URL: {e}"),
            })?;

        self.circuit.check()?;

        let result = self
            .http
            .get(url)
            .header("x-service-api-key", self.service_api_key.expose_secret())
            .query(&[("amount", amount.to_string())])
            .send()
            .await;

        match result {
            Ok(response) if response.status().is_success() => {
                self.circuit.record_success();
                Ok(response.json().await?)
            }
            Ok(response) => {
                self.circuit.record_failure();
                let status = response.status().as_u16();
                let message = response.text().await.unwrap_or_default();
                Err(BillingError::Api { status, message })
            }
            Err(e) => {
                self.circuit.record_failure();
                Err(BillingError::Request(e))
            }
        }
    }

    /// Deduct credits from an entity's balance
    ///
    /// POST `/credits/:appId/:entityType/:entityId/deduct`
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails or Aether returns an error
    pub async fn deduct_credits(
        &self,
        entity_type: &str,
        entity_id: &str,
        request: &CreditDeductRequest,
    ) -> Result<CreditDeductResponse, BillingError> {
        let url = self
            .base_url
            .join(&format!("credits/{}/{entity_type}/{entity_id}/deduct", self.app_id))
            .map_err(|e| BillingError::Api {
                status: 0,
                message: format!("invalid URL: {e}"),
            })?;

        self.circuit.check()?;

        let result = self
            .http
            .post(url)
            .header("x-service-api-key", self.service_api_key.expose_secret())
            .json(request)
            .send()
            .await;

        match result {
            Ok(response) if response.status().is_success() => {
                self.circuit.record_success();
                Ok(response.json().await?)
            }
            Ok(response) => {
                self.circuit.record_failure();
                let status = response.status().as_u16();
                let message = response.text().await.unwrap_or_default();
                Err(BillingError::Api { status, message })
            }
            Err(e) => {
                self.circuit.record_failure();
                Err(BillingError::Request(e))
            }
        }
    }

    /// Check connectivity to Aether
    ///
    /// # Errors
    ///
    /// Returns an error if Aether is unreachable
    pub async fn health(&self) -> Result<(), BillingError> {
        let url = self.base_url.join("health").map_err(|e| BillingError::Api {
            status: 0,
            message: format!("invalid URL: {e}"),
        })?;

        let response = self
            .http
            .get(url)
            .header("x-service-api-key", self.service_api_key.expose_secret())
            .send()
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status().as_u16();
            let message = response.text().await.unwrap_or_default();
            Err(BillingError::Api { status, message })
        }
    }
}

impl std::fmt::Debug for AetherClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AetherClient")
            .field("base_url", &self.base_url)
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_client(base_url: &str) -> AetherClient {
        AetherClient::new(
            Url::parse(base_url).unwrap(),
            "test-app".to_owned(),
            SecretString::from("test-key".to_owned()),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn record_usage_sends_correct_request() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/usage/test-app/user/usr_123/input_tokens/record"))
            .and(header("x-service-api-key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "billingAccountId": "ba_123",
                "meterId": "meter_456",
                "eventId": "evt_789"
            })))
            .mount(&server)
            .await;

        let client = test_client(&format!("{}/", server.uri()));

        let result = client
            .record_usage("user", "usr_123", "input_tokens", 1500.0, "idem-1", HashMap::new())
            .await;

        assert!(result.is_ok());
        assert!(result.unwrap().accepted);
    }

    #[tokio::test]
    async fn check_usage_returns_allowed() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/usage/test-app/user/usr_123/input_tokens/check"))
            .and(header("x-service-api-key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "allowed": true,
                "current": 5000.0,
                "limit": 100_000.0
            })))
            .mount(&server)
            .await;

        let client = test_client(&format!("{}/", server.uri()));

        let result = client.check_usage("user", "usr_123", "input_tokens", 1000.0).await;

        assert!(result.is_ok());
        let resp = result.unwrap();
        assert!(resp.allowed);
        assert!((resp.current_usage - 5000.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn check_entitlement_returns_access() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/entitlements/test-app/user/usr_123/api_access/check"))
            .and(header("x-service-api-key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "hasEntitlement": true,
                "version": 42
            })))
            .mount(&server)
            .await;

        let client = test_client(&format!("{}/", server.uri()));

        let result = client.check_entitlement("user", "usr_123", "api_access").await;

        assert!(result.is_ok());
        let resp = result.unwrap();
        assert!(resp.has_access);
        assert_eq!(resp.entitlement_version, Some(42));
    }

    #[tokio::test]
    async fn api_error_returns_billing_error() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/entitlements/test-app/user/usr_123/api_access/check"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal server error"))
            .mount(&server)
            .await;

        let client = test_client(&format!("{}/", server.uri()));

        let result = client.check_entitlement("user", "usr_123", "api_access").await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BillingError::Api { status: 500, .. }));
    }

    #[tokio::test]
    async fn health_check_succeeds() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = test_client(&format!("{}/", server.uri()));

        assert!(client.health().await.is_ok());
    }

    #[tokio::test]
    async fn get_entitlements_returns_list() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/entitlements/test-app/user/usr_123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "billingAccountId": "ba_123",
                "entityType": "user",
                "entityId": "usr_123",
                "entitlementVersion": 5,
                "entitlements": [
                    {
                        "id": "ent_1",
                        "appId": "test-app",
                        "featureKey": "api_access",
                        "value": 1,
                        "source": "subscription",
                        "validFrom": "2025-01-01T00:00:00Z",
                        "validUntil": null
                    },
                    {
                        "id": "ent_2",
                        "appId": "test-app",
                        "featureKey": "premium_models",
                        "value": 0,
                        "source": "subscription",
                        "validFrom": "2025-01-01T00:00:00Z",
                        "validUntil": null
                    }
                ]
            })))
            .mount(&server)
            .await;

        let client = test_client(&format!("{}/", server.uri()));

        let result = client.get_entitlements("user", "usr_123").await;

        assert!(result.is_ok());
        let resp = result.unwrap();
        assert_eq!(resp.entitlements.len(), 2);
        assert_eq!(resp.entitlements[0].feature_key, "api_access");
        assert_eq!(resp.entitlement_version, Some(5));
    }
}
