use std::collections::HashMap;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use url::Url;

use crate::circuit::CircuitBreaker;
use crate::error::BillingError;
use crate::spend::{SpendCharge, SpendCheck, SpendPolicy, SpendTarget, usd_to_credits};
use crate::types::{
    CheckUsageResponse, CreditCheckResponse, CreditDeductRequest, CreditDeductResponse, EntitlementCheckResponse,
    EntitlementsResponse, RecordUsageRequest, RecordUsageResponse,
};

/// Async HTTP client for the Aether billing API
///
/// The client is shared across all requests; the `app_id` a call bills
/// against is passed per call so one gateway can meter multiple products
/// (each request resolves its own product from the API key).
#[derive(Clone)]
pub struct AetherClient {
    http: reqwest::Client,
    base_url: Url,
    service_api_key: SecretString,
    circuit: CircuitBreaker,
    spend_policy: SpendPolicy,
}

impl AetherClient {
    /// Create a new Aether client
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be built
    pub fn new(base_url: Url, service_api_key: SecretString) -> Result<Self, BillingError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(BillingError::Request)?;

        Ok(Self {
            http,
            base_url,
            service_api_key,
            circuit: CircuitBreaker::new(),
            spend_policy: SpendPolicy::default(),
        })
    }

    /// Route each product's spend per `policy` in [`Self::check_spend`] and
    /// [`Self::settle_spend`] (every product bills the credit wallet by default)
    #[must_use]
    pub fn with_spend_policy(mut self, policy: SpendPolicy) -> Self {
        self.spend_policy = policy;
        self
    }

    /// Check whether an estimated managed spend is covered before serving it
    ///
    /// A wallet product checks its pooled credit balance (USD); an included
    /// allowance product checks its allowance meter (credits) and never the
    /// wallet
    ///
    /// # Errors
    ///
    /// Returns an error if Aether is unreachable or returns an error, for the
    /// caller's fail mode to decide
    pub async fn check_spend(
        &self,
        app_id: &str,
        entity_type: &str,
        entity_id: &str,
        estimated_usd: f64,
    ) -> Result<SpendCheck, BillingError> {
        match self.spend_policy.target(app_id) {
            SpendTarget::Wallet => {
                let response = self
                    .check_credits(app_id, entity_type, entity_id, estimated_usd)
                    .await?;
                Ok(if response.sufficient {
                    SpendCheck::Allowed
                } else {
                    SpendCheck::InsufficientCredits {
                        balance: response.balance,
                    }
                })
            }
            SpendTarget::Allowance { meter_key } => {
                let response = self
                    .check_usage(app_id, entity_type, entity_id, meter_key, usd_to_credits(estimated_usd))
                    .await?;
                Ok(if response.allowed {
                    SpendCheck::Allowed
                } else {
                    SpendCheck::AllowanceExhausted
                })
            }
        }
    }

    /// Charge the actual managed spend of a served request
    ///
    /// A wallet product is deducted from its pooled credit balance (USD); an
    /// included allowance product records the credits on its allowance meter
    /// and never touches the wallet
    ///
    /// # Errors
    ///
    /// Returns an error if Aether is unreachable or returns an error
    pub async fn settle_spend(
        &self,
        app_id: &str,
        entity_type: &str,
        entity_id: &str,
        charge: &SpendCharge,
    ) -> Result<(), BillingError> {
        match self.spend_policy.target(app_id) {
            SpendTarget::Wallet => {
                let request = CreditDeductRequest {
                    amount: charge.amount_usd,
                    description: Some(charge.description.clone()),
                    idempotency_key: Some(charge.idempotency_key.clone()),
                    reference_type: Some(charge.reference_type.clone()),
                    reference_id: None,
                };
                self.deduct_credits(app_id, entity_type, entity_id, &request).await?;
            }
            SpendTarget::Allowance { meter_key } => {
                let metadata = HashMap::from([
                    ("description".to_owned(), charge.description.clone()),
                    ("referenceType".to_owned(), charge.reference_type.clone()),
                    ("amountUsd".to_owned(), charge.amount_usd.to_string()),
                ]);
                // Suffixed so the key never collides with the token and
                // request meters a request records under the same id
                self.record_usage(
                    app_id,
                    entity_type,
                    entity_id,
                    meter_key,
                    usd_to_credits(charge.amount_usd),
                    &format!("{}-allowance", charge.idempotency_key),
                    metadata,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Record usage against a meter
    ///
    /// POST `/usage/:appId/:entityType/:entityId/:meterKey/record`
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails or Aether returns an error
    #[allow(clippy::too_many_arguments)]
    pub async fn record_usage(
        &self,
        app_id: &str,
        entity_type: &str,
        entity_id: &str,
        meter_key: &str,
        delta: f64,
        idempotency_key: &str,
        metadata: HashMap<String, String>,
    ) -> Result<RecordUsageResponse, BillingError> {
        let url = self
            .base_url
            .join(&format!("usage/{app_id}/{entity_type}/{entity_id}/{meter_key}/record"))
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
        app_id: &str,
        entity_type: &str,
        entity_id: &str,
        meter_key: &str,
        additional_usage: f64,
    ) -> Result<CheckUsageResponse, BillingError> {
        let url = self
            .base_url
            .join(&format!("usage/{app_id}/{entity_type}/{entity_id}/{meter_key}/check"))
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
        app_id: &str,
        entity_type: &str,
        entity_id: &str,
        feature_key: &str,
    ) -> Result<EntitlementCheckResponse, BillingError> {
        let url = self
            .base_url
            .join(&format!(
                "entitlements/{app_id}/{entity_type}/{entity_id}/{feature_key}/check"
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
        app_id: &str,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<EntitlementsResponse, BillingError> {
        let url = self
            .base_url
            .join(&format!("entitlements/{app_id}/{entity_type}/{entity_id}"))
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
        app_id: &str,
        entity_type: &str,
        entity_id: &str,
        amount: f64,
    ) -> Result<CreditCheckResponse, BillingError> {
        let url = self
            .base_url
            .join(&format!("credits/{app_id}/{entity_type}/{entity_id}/check"))
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
        app_id: &str,
        entity_type: &str,
        entity_id: &str,
        request: &CreditDeductRequest,
    ) -> Result<CreditDeductResponse, BillingError> {
        let url = self
            .base_url
            .join(&format!("credits/{app_id}/{entity_type}/{entity_id}/deduct"))
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
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spend::{SpendCharge, SpendCheck, SpendPolicy};

    use wiremock::matchers::{body_partial_json, header, method, path, path_regex, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_client(base_url: &str) -> AetherClient {
        AetherClient::new(Url::parse(base_url).unwrap(), SecretString::from("test-key".to_owned())).unwrap()
    }

    #[tokio::test]
    async fn per_call_app_id_scopes_the_request_path() {
        // The app billed is a per-call parameter, not a client-wide constant, so
        // one shared client meters different products. A call for "facet" must
        // hit the facet app path, never a baked-in default.
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/usage/facet/user/usr_fct/requests/record"))
            .and(header("x-service-api-key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "billingAccountId": "ba_fct",
                "meterId": "meter_1",
                "eventId": "evt_1"
            })))
            .expect(1)
            .named("usage must route to the per-call app_id (facet)")
            .mount(&server)
            .await;

        let client = test_client(&format!("{}/", server.uri()));

        let result = client
            .record_usage("facet", "user", "usr_fct", "requests", 1.0, "idem-fct", HashMap::new())
            .await;

        assert!(result.is_ok());
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
            .record_usage(
                "test-app",
                "user",
                "usr_123",
                "input_tokens",
                1500.0,
                "idem-1",
                HashMap::new(),
            )
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

        let result = client
            .check_usage("test-app", "user", "usr_123", "input_tokens", 1000.0)
            .await;

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

        let result = client
            .check_entitlement("test-app", "user", "usr_123", "api_access")
            .await;

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

        let result = client
            .check_entitlement("test-app", "user", "usr_123", "api_access")
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BillingError::Api { status: 500, .. }));
    }

    #[tokio::test]
    async fn check_credits_sufficient() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/credits/test-app/user/usr_123/check"))
            .and(header("x-service-api-key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sufficient": true,
                "balance": 50.0
            })))
            .mount(&server)
            .await;

        let client = test_client(&format!("{}/", server.uri()));

        let result = client.check_credits("test-app", "user", "usr_123", 10.0).await;

        assert!(result.is_ok());
        let resp = result.unwrap();
        assert!(resp.sufficient);
        assert!((resp.balance - 50.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn check_credits_insufficient() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/credits/test-app/user/usr_123/check"))
            .and(header("x-service-api-key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sufficient": false,
                "balance": 0.5
            })))
            .mount(&server)
            .await;

        let client = test_client(&format!("{}/", server.uri()));

        let result = client.check_credits("test-app", "user", "usr_123", 10.0).await;

        assert!(result.is_ok());
        let resp = result.unwrap();
        assert!(!resp.sufficient);
        assert!((resp.balance - 0.5).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn check_credits_unreachable_returns_request_error() {
        // Point at a port that is not listening to simulate Aether being down
        let client = test_client("http://127.0.0.1:1/");

        let result = client.check_credits("test-app", "user", "usr_123", 10.0).await;

        assert!(result.is_err());
        // The error should be a Request variant (network error), which is
        // what the fail_mode logic in LlmState branches on
        assert!(matches!(result.unwrap_err(), BillingError::Request(_)));
    }

    #[tokio::test]
    async fn check_credits_server_error_returns_api_error() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/credits/test-app/user/usr_123/check"))
            .respond_with(ResponseTemplate::new(503).set_body_string("service unavailable"))
            .mount(&server)
            .await;

        let client = test_client(&format!("{}/", server.uri()));

        let result = client.check_credits("test-app", "user", "usr_123", 10.0).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), BillingError::Api { status: 503, .. }));
    }

    fn posture_client(base_url: &str) -> AetherClient {
        test_client(base_url).with_spend_policy(SpendPolicy::new(HashMap::from([(
            "myfi".to_owned(),
            "ai_credits".to_owned(),
        )])))
    }

    /// Fails the test if any wallet endpoint (credit check or deduct) is hit
    async fn forbid_wallet_calls(server: &MockServer) {
        Mock::given(path_regex(r"^/credits/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .named("an allowance product must never touch the credit wallet")
            .mount(server)
            .await;
    }

    fn charge(amount_usd: f64) -> SpendCharge {
        SpendCharge {
            amount_usd,
            description: "anthropic/claude-haiku-5".to_owned(),
            idempotency_key: "req-1".to_owned(),
            reference_type: "completion".to_owned(),
        }
    }

    #[tokio::test]
    async fn check_spend_for_an_allowance_product_checks_its_meter_in_credits() {
        let server = MockServer::start().await;
        forbid_wallet_calls(&server).await;

        // $2.50 estimated retail spend is 250 credits
        Mock::given(method("GET"))
            .and(path("/usage/myfi/organization/org_1/ai_credits/check"))
            .and(query_param("additionalUsage", "250"))
            .and(header("x-service-api-key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "allowed": true, "current": 10.0, "limit": 300, "remaining": 290
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = posture_client(&format!("{}/", server.uri()));

        let check = client.check_spend("myfi", "organization", "org_1", 2.5).await.unwrap();

        assert_eq!(check, SpendCheck::Allowed);
    }

    #[tokio::test]
    async fn check_spend_reports_an_exhausted_allowance() {
        let server = MockServer::start().await;
        forbid_wallet_calls(&server).await;

        Mock::given(method("GET"))
            .and(path("/usage/myfi/organization/org_1/ai_credits/check"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "allowed": false, "current": 25.0, "limit": 25, "remaining": 0
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = posture_client(&format!("{}/", server.uri()));

        let check = client.check_spend("myfi", "organization", "org_1", 0.01).await.unwrap();

        assert_eq!(check, SpendCheck::AllowanceExhausted);
    }

    #[tokio::test]
    async fn check_spend_for_a_wallet_product_checks_credits_in_usd() {
        let server = MockServer::start().await;

        Mock::given(path_regex(r"^/usage/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .named("a wallet product must not check an allowance meter")
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/credits/facet/user/usr_1/check"))
            .and(query_param("amount", "2.5"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sufficient": false, "balance": 1.25
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = posture_client(&format!("{}/", server.uri()));

        let check = client.check_spend("facet", "user", "usr_1", 2.5).await.unwrap();

        assert_eq!(check, SpendCheck::InsufficientCredits { balance: 1.25 });
    }

    #[tokio::test]
    async fn settle_spend_records_allowance_credits_instead_of_deducting() {
        let server = MockServer::start().await;
        forbid_wallet_calls(&server).await;

        Mock::given(method("POST"))
            .and(path("/usage/myfi/organization/org_1/ai_credits/record"))
            .and(body_partial_json(serde_json::json!({
                "delta": 1.5,
                "idempotencyKey": "req-1-allowance",
                "metadata": {
                    "description": "anthropic/claude-haiku-5",
                    "referenceType": "completion",
                    "amountUsd": "0.015"
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "billingAccountId": "ba_1", "meterId": "m_1", "eventId": "e_1"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = posture_client(&format!("{}/", server.uri()));

        client
            .settle_spend("myfi", "organization", "org_1", &charge(0.015))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn settle_spend_deducts_a_wallet_product_in_usd() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/credits/facet/user/usr_1/deduct"))
            .and(body_partial_json(serde_json::json!({
                "amount": 0.015,
                "idempotencyKey": "req-1",
                "referenceType": "completion",
                "description": "anthropic/claude-haiku-5"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true, "balance": 0.985
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = posture_client(&format!("{}/", server.uri()));

        client
            .settle_spend("facet", "user", "usr_1", &charge(0.015))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn check_spend_surfaces_an_unreachable_aether_for_the_fail_mode() {
        let client = posture_client("http://127.0.0.1:1/");

        let result = client.check_spend("myfi", "organization", "org_1", 0.5).await;

        assert!(matches!(result, Err(BillingError::Request(_))));
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

        let result = client.get_entitlements("test-app", "user", "usr_123").await;

        assert!(result.is_ok());
        let resp = result.unwrap();
        assert_eq!(resp.entitlements.len(), 2);
        assert_eq!(resp.entitlements[0].feature_key, "api_access");
        assert_eq!(resp.entitlement_version, Some(5));
    }
}
