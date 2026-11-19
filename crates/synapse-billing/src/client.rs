use std::collections::HashMap;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use url::Url;

use crate::circuit::CircuitBreaker;
use crate::error::BillingError;
use crate::paying::{PayingCustomerGate, TIER_FEATURE_KEY, WalletAccess, is_paid_tier};
use crate::spend::{SpendCharge, SpendCheck, SpendPolicy, SpendSettlement, SpendTarget, usd_to_credits};
use crate::types::{
    CheckUsageResponse, CreditCheckResponse, CreditDeductRequest, CreditDeductResponse, CreditFundingResponse,
    EntitlementCheckResponse, EntitlementsResponse, RecordUsageRequest, RecordUsageResponse,
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
    paying_customer_gate: Option<PayingCustomerGate>,
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
            paying_customer_gate: None,
        })
    }

    /// Whether managed spend billed to `app_id` is charged to the pooled
    /// credit wallet (rather than an included allowance meter)
    #[must_use]
    pub fn bills_wallet(&self, app_id: &str) -> bool {
        self.spend_policy.target(app_id) == SpendTarget::Wallet
    }

    /// Settle a response with the circuit breaker and decode its body
    ///
    /// Only an unreachable or failing Aether (a transport error, a 5xx, a 408
    /// or a 429) counts toward opening the circuit. Any other 4xx means Aether
    /// answered and refused that one request, so it must not open the circuit
    /// for every caller: wallet-billed spend checks fail closed while the
    /// circuit is open
    async fn read_response<T: serde::de::DeserializeOwned>(
        &self,
        result: Result<reqwest::Response, reqwest::Error>,
    ) -> Result<T, BillingError> {
        match result {
            Ok(response) if response.status().is_success() => {
                self.circuit.record_success();
                Ok(response.json().await?)
            }
            Ok(response) => {
                let status = response.status();
                if counts_as_outage(status) {
                    self.circuit.record_failure();
                } else {
                    self.circuit.record_success();
                }
                let message = response.text().await.unwrap_or_default();
                Err(BillingError::Api {
                    status: status.as_u16(),
                    message,
                })
            }
            Err(e) => {
                self.circuit.record_failure();
                Err(BillingError::Request(e))
            }
        }
    }

    /// Route each product's spend per `policy` in [`Self::check_spend`] and
    /// [`Self::settle_spend`] (every product bills the credit wallet by default)
    #[must_use]
    pub fn with_spend_policy(mut self, policy: SpendPolicy) -> Self {
        self.spend_policy = policy;
        self
    }

    /// Serve wallet-billed spend only to paying customers, deciding through
    /// (and caching in) `gate`
    ///
    /// With the gate set, [`Self::check_spend`] refuses a wallet product's
    /// spend with [`SpendCheck::PaymentRequired`] unless the entity is on a
    /// paid tier of the product or has bought credits. Included allowance
    /// products are never gated
    #[must_use]
    pub fn with_paying_customer_gate(mut self, gate: PayingCustomerGate) -> Self {
        self.paying_customer_gate = Some(gate);
        self
    }

    /// Check whether an estimated managed spend is covered before serving it
    ///
    /// A wallet product checks its pooled credit balance (USD), after the
    /// paying-customer gate when one is set; an included allowance product
    /// checks its allowance meter (credits) and never the wallet
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
        self.check_spend_on(app_id, entity_type, entity_id, estimated_usd, false)
            .await
    }

    /// [`Self::check_spend`] for a chat request, which a card-verified free
    /// account may also spend its wallet on when every model that may serve
    /// it is the product's free model (`on_free_model`)
    ///
    /// # Errors
    ///
    /// Returns an error if Aether is unreachable or returns an error, for the
    /// caller's fail mode to decide
    pub async fn check_spend_on(
        &self,
        app_id: &str,
        entity_type: &str,
        entity_id: &str,
        estimated_usd: f64,
        on_free_model: bool,
    ) -> Result<SpendCheck, BillingError> {
        match self.spend_policy.target(app_id) {
            SpendTarget::Wallet => {
                if let Some(gate) = &self.paying_customer_gate {
                    let allowed = match self.wallet_access_with(gate, app_id, entity_type, entity_id).await? {
                        WalletAccess::Paying => true,
                        WalletAccess::FreeAllowance => on_free_model,
                        WalletAccess::Denied => false,
                    };
                    if !allowed {
                        return Ok(SpendCheck::PaymentRequired);
                    }
                }
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

    /// How an entity may spend a product's credit wallet, or `None` when
    /// wallet spend is not gated for the product (an included allowance
    /// product, or no paying-customer gate)
    ///
    /// # Errors
    ///
    /// Returns an error if Aether is unreachable or returns an error
    pub async fn wallet_access(
        &self,
        app_id: &str,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<Option<WalletAccess>, BillingError> {
        let Some(gate) = &self.paying_customer_gate else {
            return Ok(None);
        };
        if self.spend_policy.target(app_id) != SpendTarget::Wallet {
            return Ok(None);
        }
        self.wallet_access_with(gate, app_id, entity_type, entity_id)
            .await
            .map(Some)
    }

    /// How an entity may spend a product's wallet: paying (a paid tier, or
    /// bought credits), the card-verified free allowance, or not at all
    ///
    /// The tier is read first and funding only for a free tier, so a paid
    /// tier costs one Aether call. A decision is cached in `gate`; a failure
    /// to reach Aether is returned for the caller's fail mode and never
    /// cached
    async fn wallet_access_with(
        &self,
        gate: &PayingCustomerGate,
        app_id: &str,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<WalletAccess, BillingError> {
        if let Some(access) = gate.cached(app_id, entity_type, entity_id) {
            return Ok(access);
        }

        let tier = self
            .check_entitlement(app_id, entity_type, entity_id, TIER_FEATURE_KEY)
            .await?;
        let access = if is_paid_tier(tier.value.as_ref()) {
            WalletAccess::Paying
        } else {
            let funding = self.credit_funding(app_id, entity_type, entity_id).await?;
            if funding.is_paying_customer {
                WalletAccess::Paying
            } else if funding.card_verified {
                WalletAccess::FreeAllowance
            } else {
                WalletAccess::Denied
            }
        };

        gate.remember(app_id, entity_type, entity_id, access);
        Ok(access)
    }

    /// Charge the actual managed spend of a served request
    ///
    /// A wallet product is deducted from its pooled credit balance (USD); an
    /// included allowance product records the credits on its allowance meter
    /// and never touches the wallet. Answers where the charge went, so a
    /// wallet debit can be reported (and later reversed) by its idempotency key
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
    ) -> Result<SpendSettlement, BillingError> {
        match self.spend_policy.target(app_id) {
            SpendTarget::Wallet => {
                // The request was already served, so the debit is a
                // settlement: Aether records it even when it takes the balance
                // negative instead of refusing it and losing the charge
                let request = CreditDeductRequest {
                    amount: charge.amount_usd,
                    description: Some(charge.description.clone()),
                    idempotency_key: Some(charge.idempotency_key.clone()),
                    reference_type: Some(charge.reference_type.clone()),
                    reference_id: None,
                    settle: true,
                };
                if let Err(e) = self.deduct_credits(app_id, entity_type, entity_id, &request).await {
                    log_unsettled_debit(app_id, entity_type, entity_id, charge, &e);
                    return Err(e);
                }
                Ok(SpendSettlement::Wallet)
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
                Ok(SpendSettlement::Allowance)
            }
        }
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

        self.read_response(result).await
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

        self.read_response(result).await
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

        self.read_response(result).await
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

        self.read_response(result).await
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

        self.read_response(result).await
    }

    /// Whether an entity has ever paid real money for credits
    ///
    /// GET `/credits/:appId/:entityType/:entityId/funding`
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails, Aether returns an error,
    /// or the response is malformed
    pub async fn credit_funding(
        &self,
        app_id: &str,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<CreditFundingResponse, BillingError> {
        let url = self
            .base_url
            .join(&format!("credits/{app_id}/{entity_type}/{entity_id}/funding"))
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

        self.read_response(result).await
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
        // The ledger stores 4 decimal places, so round the charge up to it:
        // left to Aether, a sub-$0.00005 request would round to a free $0
        let rounded = CreditDeductRequest {
            amount: round_up_to_ledger_precision(request.amount),
            ..request.clone()
        };
        let request = &rounded;

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

        self.read_response(result).await
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

/// Whether a non-success Aether status means Aether is unavailable, as
/// opposed to refusing one well-formed request
fn counts_as_outage(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

/// Record a served request whose wallet debit did not land
///
/// The upstream spend already happened, so a lost debit is unbilled usage
/// that must be reconciled by hand. Logged once at error level with every
/// field needed to replay the debit (the idempotency key makes a replay
/// safe); never retried here, so a debit Aether refuses cannot loop
fn log_unsettled_debit(app_id: &str, entity_type: &str, entity_id: &str, charge: &SpendCharge, error: &BillingError) {
    let status = match error {
        BillingError::Api { status, .. } => Some(*status),
        _ => None,
    };
    let reason = match status {
        // An Aether that predates settlement debits may reject the `settle`
        // field outright, or ignore it and refuse a debit the balance cannot
        // cover
        Some(400 | 422) => "Aether rejected the settlement debit request (settle field unsupported?)",
        Some(402) => "Aether refused the settlement debit for insufficient balance (settle not honored)",
        Some(_) => "Aether returned an error for the settlement debit",
        None => "Aether unreachable for the settlement debit",
    };
    tracing::error!(
        error = %error,
        status,
        app_id,
        entity_type,
        entity_id,
        amount_usd = charge.amount_usd,
        idempotency_key = %charge.idempotency_key,
        description = %charge.description,
        reference_type = %charge.reference_type,
        reconcile = true,
        "unsettled wallet debit: {reason}"
    );
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
    use crate::paying::PayingCustomerGate;
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

        let settlement = client
            .settle_spend("myfi", "organization", "org_1", &charge(0.015))
            .await
            .unwrap();
        assert_eq!(settlement, SpendSettlement::Allowance);
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
                "description": "anthropic/claude-haiku-5",
                "settle": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true, "balance": 0.985
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = posture_client(&format!("{}/", server.uri()));

        let settlement = client
            .settle_spend("facet", "user", "usr_1", &charge(0.015))
            .await
            .unwrap();
        assert_eq!(settlement, SpendSettlement::Wallet);
    }

    #[tokio::test]
    async fn a_rejected_settlement_debit_is_attempted_once_and_surfaced() {
        // An Aether that predates settlement debits may reject the unknown
        // field: the charge is logged for reconciliation, never retried
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/credits/facet/user/usr_1/deduct"))
            .respond_with(ResponseTemplate::new(422).set_body_string("unknown property settle"))
            .expect(1)
            .mount(&server)
            .await;

        let client = posture_client(&format!("{}/", server.uri()));

        let result = client.settle_spend("facet", "user", "usr_1", &charge(0.015)).await;

        assert!(matches!(result, Err(BillingError::Api { status: 422, .. })));
    }

    #[test]
    fn bills_wallet_follows_the_spend_policy() {
        let client = posture_client("http://127.0.0.1:1/");

        assert!(client.bills_wallet("facet"));
        assert!(client.bills_wallet("synapse"));
        assert!(!client.bills_wallet("myfi"));
    }

    #[tokio::test]
    async fn client_errors_never_open_the_circuit() {
        // A refused request (e.g. a rejected debit) proves Aether is up, so a
        // run of them must not open the circuit and fail every wallet-billed
        // spend check closed
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/credits/facet/user/usr_1/deduct"))
            .respond_with(ResponseTemplate::new(422))
            .expect(5)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/credits/facet/user/usr_1/check"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sufficient": true, "balance": 1.0
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = posture_client(&format!("{}/", server.uri()));
        for _ in 0..5 {
            let _ = client.settle_spend("facet", "user", "usr_1", &charge(0.015)).await;
        }

        let check = client.check_spend("facet", "user", "usr_1", 0.5).await;

        assert!(matches!(check, Ok(SpendCheck::Allowed)));
    }

    #[tokio::test]
    async fn server_errors_still_open_the_circuit() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/credits/facet/user/usr_1/check"))
            .respond_with(ResponseTemplate::new(503))
            .expect(3)
            .mount(&server)
            .await;

        let client = posture_client(&format!("{}/", server.uri()));
        for _ in 0..3 {
            let _ = client.check_spend("facet", "user", "usr_1", 0.5).await;
        }

        let check = client.check_spend("facet", "user", "usr_1", 0.5).await;

        assert!(matches!(check, Err(BillingError::CircuitOpen)));
    }

    #[tokio::test]
    async fn check_spend_surfaces_an_unreachable_aether_for_the_fail_mode() {
        let client = posture_client("http://127.0.0.1:1/");

        let result = client.check_spend("myfi", "organization", "org_1", 0.5).await;

        assert!(matches!(result, Err(BillingError::Request(_))));
    }

    #[tokio::test]
    async fn credit_funding_reads_a_paying_customer() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/credits/facet/user/usr_1/funding"))
            .and(header("x-service-api-key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "billingAccountId": "ba_1", "paidCreditsUsd": 20, "isPayingCustomer": true
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = test_client(&format!("{}/", server.uri()));

        let funding = client.credit_funding("facet", "user", "usr_1").await.unwrap();

        assert_eq!(
            funding,
            CreditFundingResponse {
                billing_account_id: Some("ba_1".to_owned()),
                paid_credits_usd: 20.0,
                is_paying_customer: true,
                card_verified: false,
            }
        );
    }

    #[tokio::test]
    async fn credit_funding_reads_an_unknown_entity_as_never_paid() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/credits/facet/user/usr_1/funding"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "billingAccountId": null, "paidCreditsUsd": 0, "isPayingCustomer": false
            })))
            .mount(&server)
            .await;

        let client = test_client(&format!("{}/", server.uri()));

        let funding = client.credit_funding("facet", "user", "usr_1").await.unwrap();

        assert_eq!(funding.billing_account_id, None);
        assert!(!funding.is_paying_customer);
    }

    #[tokio::test]
    async fn credit_funding_rejects_a_malformed_answer() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/credits/facet/user/usr_1/funding"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "billingAccountId": "ba_1", "paidCreditsUsd": 20
            })))
            .mount(&server)
            .await;

        let client = test_client(&format!("{}/", server.uri()));

        assert!(client.credit_funding("facet", "user", "usr_1").await.is_err());
    }

    fn gated_client(base_url: &str) -> AetherClient {
        posture_client(base_url).with_paying_customer_gate(PayingCustomerGate::new())
    }

    async fn mount_tier(server: &MockServer, tier: serde_json::Value, times: u64) {
        Mock::given(method("GET"))
            .and(path("/entitlements/facet/user/usr_1/tier/check"))
            .and(header("x-service-api-key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "hasEntitlement": !tier.is_null(), "value": tier, "version": 3
            })))
            .expect(times)
            .mount(server)
            .await;
    }

    async fn mount_funding(server: &MockServer, is_paying: bool, times: u64) {
        Mock::given(method("GET"))
            .and(path("/credits/facet/user/usr_1/funding"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "billingAccountId": "ba_1",
                "paidCreditsUsd": if is_paying { 10.0 } else { 0.0 },
                "isPayingCustomer": is_paying
            })))
            .expect(times)
            .mount(server)
            .await;
    }

    async fn mount_balance(server: &MockServer, times: u64) {
        Mock::given(method("GET"))
            .and(path("/credits/facet/user/usr_1/check"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sufficient": true, "balance": 5.0
            })))
            .expect(times)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn a_free_tier_that_never_paid_is_refused_before_the_balance_check() {
        let server = MockServer::start().await;
        mount_tier(&server, serde_json::json!("free"), 1).await;
        mount_funding(&server, false, 1).await;
        // Free grants in the wallet must not make it spendable
        mount_balance(&server, 0).await;

        let client = gated_client(&format!("{}/", server.uri()));

        let check = client.check_spend("facet", "user", "usr_1", 0.5).await.unwrap();

        assert_eq!(check, SpendCheck::PaymentRequired);
    }

    #[tokio::test]
    async fn an_absent_tier_that_never_paid_is_refused() {
        let server = MockServer::start().await;
        mount_tier(&server, serde_json::Value::Null, 1).await;
        mount_funding(&server, false, 1).await;
        mount_balance(&server, 0).await;

        let client = gated_client(&format!("{}/", server.uri()));

        let check = client.check_spend("facet", "user", "usr_1", 0.5).await.unwrap();

        assert_eq!(check, SpendCheck::PaymentRequired);
    }

    async fn mount_card_verified_funding(server: &MockServer, times: u64) {
        Mock::given(method("GET"))
            .and(path("/credits/facet/user/usr_1/funding"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "billingAccountId": "ba_1",
                "paidCreditsUsd": 0.0,
                "isPayingCustomer": false,
                "cardVerified": true
            })))
            .expect(times)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn a_card_verified_free_account_may_spend_on_the_free_model() {
        let server = MockServer::start().await;
        mount_tier(&server, serde_json::json!("free"), 1).await;
        mount_card_verified_funding(&server, 1).await;
        mount_balance(&server, 1).await;

        let client = gated_client(&format!("{}/", server.uri()));

        let check = client
            .check_spend_on("facet", "user", "usr_1", 0.5, true)
            .await
            .unwrap();

        assert_eq!(check, SpendCheck::Allowed);
    }

    #[tokio::test]
    async fn a_card_verified_free_account_is_refused_any_other_model_or_modality() {
        let server = MockServer::start().await;
        mount_tier(&server, serde_json::json!("free"), 1).await;
        mount_card_verified_funding(&server, 1).await;
        mount_balance(&server, 0).await;

        let client = gated_client(&format!("{}/", server.uri()));

        // A chat request a paid model may serve
        assert_eq!(
            client
                .check_spend_on("facet", "user", "usr_1", 0.5, false)
                .await
                .unwrap(),
            SpendCheck::PaymentRequired
        );
        // Images, speech, and embeddings never spend the free allowance
        assert_eq!(
            client.check_spend("facet", "user", "usr_1", 0.5).await.unwrap(),
            SpendCheck::PaymentRequired
        );
    }

    #[tokio::test]
    async fn wallet_access_reports_the_free_allowance() {
        let server = MockServer::start().await;
        mount_tier(&server, serde_json::json!("free"), 1).await;
        mount_card_verified_funding(&server, 1).await;

        let client = gated_client(&format!("{}/", server.uri()));

        assert_eq!(
            client.wallet_access("facet", "user", "usr_1").await.unwrap(),
            Some(WalletAccess::FreeAllowance)
        );
        // Cached: no second Aether round trip
        assert_eq!(
            client.wallet_access("facet", "user", "usr_1").await.unwrap(),
            Some(WalletAccess::FreeAllowance)
        );
    }

    #[tokio::test]
    async fn wallet_access_is_none_without_a_gate() {
        let server = MockServer::start().await;
        let client = posture_client(&format!("{}/", server.uri()));

        assert_eq!(client.wallet_access("facet", "user", "usr_1").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_free_tier_that_bought_credits_may_spend_its_balance() {
        let server = MockServer::start().await;
        mount_tier(&server, serde_json::json!("free"), 1).await;
        mount_funding(&server, true, 1).await;
        mount_balance(&server, 1).await;

        let client = gated_client(&format!("{}/", server.uri()));

        let check = client.check_spend("facet", "user", "usr_1", 0.5).await.unwrap();

        assert_eq!(check, SpendCheck::Allowed);
    }

    #[tokio::test]
    async fn a_paid_tier_skips_the_funding_check() {
        let server = MockServer::start().await;
        mount_tier(&server, serde_json::json!("team"), 1).await;
        mount_funding(&server, false, 0).await;
        mount_balance(&server, 1).await;

        let client = gated_client(&format!("{}/", server.uri()));

        let check = client.check_spend("facet", "user", "usr_1", 0.5).await.unwrap();

        assert_eq!(check, SpendCheck::Allowed);
    }

    #[tokio::test]
    async fn a_paying_customer_still_needs_the_balance() {
        let server = MockServer::start().await;
        mount_tier(&server, serde_json::json!("pro"), 1).await;
        Mock::given(method("GET"))
            .and(path("/credits/facet/user/usr_1/check"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sufficient": false, "balance": 0.1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = gated_client(&format!("{}/", server.uri()));

        let check = client.check_spend("facet", "user", "usr_1", 0.5).await.unwrap();

        assert_eq!(check, SpendCheck::InsufficientCredits { balance: 0.1 });
    }

    #[tokio::test]
    async fn the_decision_is_cached_across_requests() {
        let server = MockServer::start().await;
        mount_tier(&server, serde_json::json!("free"), 1).await;
        mount_funding(&server, true, 1).await;
        mount_balance(&server, 3).await;

        let client = gated_client(&format!("{}/", server.uri()));

        for _ in 0..3 {
            let check = client.check_spend("facet", "user", "usr_1", 0.5).await.unwrap();
            assert_eq!(check, SpendCheck::Allowed);
        }
    }

    #[tokio::test]
    async fn the_cache_is_shared_by_every_client_holding_the_gate() {
        let server = MockServer::start().await;
        mount_tier(&server, serde_json::json!("free"), 1).await;
        mount_funding(&server, false, 1).await;
        mount_balance(&server, 0).await;

        let gate = PayingCustomerGate::new();
        let chat = posture_client(&format!("{}/", server.uri())).with_paying_customer_gate(gate.clone());
        let images = posture_client(&format!("{}/", server.uri())).with_paying_customer_gate(gate);

        assert_eq!(
            chat.check_spend("facet", "user", "usr_1", 0.5).await.unwrap(),
            SpendCheck::PaymentRequired
        );
        assert_eq!(
            images.check_spend("facet", "user", "usr_1", 0.5).await.unwrap(),
            SpendCheck::PaymentRequired
        );
    }

    #[tokio::test]
    async fn an_unreachable_funding_check_is_an_error_and_never_cached() {
        let server = MockServer::start().await;
        mount_tier(&server, serde_json::json!("free"), 2).await;
        Mock::given(method("GET"))
            .and(path("/credits/facet/user/usr_1/funding"))
            .respond_with(ResponseTemplate::new(503))
            .expect(2)
            .mount(&server)
            .await;
        mount_balance(&server, 0).await;

        let client = gated_client(&format!("{}/", server.uri()));

        for _ in 0..2 {
            let result = client.check_spend("facet", "user", "usr_1", 0.5).await;
            assert!(matches!(result, Err(BillingError::Api { status: 503, .. })));
        }
    }

    #[tokio::test]
    async fn a_malformed_funding_answer_is_an_error_not_a_paying_customer() {
        let server = MockServer::start().await;
        mount_tier(&server, serde_json::json!("free"), 1).await;
        Mock::given(method("GET"))
            .and(path("/credits/facet/user/usr_1/funding"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "paidCreditsUsd": 5 })))
            .expect(1)
            .mount(&server)
            .await;
        mount_balance(&server, 0).await;

        let client = gated_client(&format!("{}/", server.uri()));

        assert!(client.check_spend("facet", "user", "usr_1", 0.5).await.is_err());
    }

    #[tokio::test]
    async fn an_unreachable_tier_check_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/entitlements/facet/user/usr_1/tier/check"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount(&server)
            .await;
        mount_funding(&server, true, 0).await;
        mount_balance(&server, 0).await;

        let client = gated_client(&format!("{}/", server.uri()));

        let result = client.check_spend("facet", "user", "usr_1", 0.5).await;

        assert!(matches!(result, Err(BillingError::Api { status: 500, .. })));
    }

    #[tokio::test]
    async fn an_allowance_product_is_never_gated() {
        let server = MockServer::start().await;
        forbid_wallet_calls(&server).await;
        Mock::given(path_regex(r"^/entitlements/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .named("an allowance product must not read its tier for the gate")
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/usage/myfi/organization/org_1/ai_credits/check"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "allowed": true, "current": 0.0, "limit": 300
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = gated_client(&format!("{}/", server.uri()));

        let check = client.check_spend("myfi", "organization", "org_1", 0.5).await.unwrap();

        assert_eq!(check, SpendCheck::Allowed);
    }

    #[tokio::test]
    async fn without_the_gate_a_wallet_product_checks_only_its_balance() {
        let server = MockServer::start().await;
        Mock::given(path_regex(r"^/entitlements/|/funding$"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        mount_balance(&server, 1).await;

        let client = posture_client(&format!("{}/", server.uri()));

        let check = client.check_spend("facet", "user", "usr_1", 0.5).await.unwrap();

        assert_eq!(check, SpendCheck::Allowed);
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

/// Ledger resolution of Aether's credit balance (`numeric(12,4)`, USD)
const LEDGER_UNITS_PER_USD: f64 = 10_000.0;

/// Round a USD charge up to the ledger's precision so no request is free
///
/// A tiny epsilon absorbs float noise (0.1 is stored as 0.1000000000000000055)
/// so an exact amount is never pushed up a whole unit
#[must_use]
pub fn round_up_to_ledger_precision(amount_usd: f64) -> f64 {
    if amount_usd <= 0.0 {
        return amount_usd;
    }
    amount_usd.mul_add(LEDGER_UNITS_PER_USD, -1e-6).ceil() / LEDGER_UNITS_PER_USD
}

#[cfg(test)]
mod ledger_precision_tests {
    use super::round_up_to_ledger_precision;

    #[test]
    fn sub_unit_charges_round_up_to_one_unit() {
        assert!((round_up_to_ledger_precision(0.000_009) - 0.0001).abs() < 1e-12);
        assert!((round_up_to_ledger_precision(0.000_049) - 0.0001).abs() < 1e-12);
    }

    #[test]
    fn exact_amounts_are_unchanged() {
        for amount in [0.1, 0.0003, 1.0, 0.9877, 12.3456] {
            assert!(
                (round_up_to_ledger_precision(amount) - amount).abs() < 1e-12,
                "{amount}"
            );
        }
    }

    #[test]
    fn partial_units_round_up() {
        assert!((round_up_to_ledger_precision(0.012_31) - 0.0124).abs() < 1e-12);
    }

    #[test]
    fn zero_and_negative_pass_through() {
        assert!(round_up_to_ledger_precision(0.0).abs() < f64::EPSILON);
        assert!((round_up_to_ledger_precision(-1.0) + 1.0).abs() < f64::EPSILON);
    }
}
