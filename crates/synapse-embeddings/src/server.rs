use secrecy::SecretString;
use synapse_config::{EmbeddingsProviderConfig, EmbeddingsProviderType, FailMode};
use synapse_core::RequestContext;

use crate::{
    error::EmbeddingsError,
    provider::{EmbeddingsProvider, openai::OpenAiEmbeddingsProvider},
    types::{EmbeddingRequest, EmbeddingResponse},
};

/// Embeddings server that routes requests to the appropriate provider
pub struct Server {
    providers: Vec<Box<dyn EmbeddingsProvider>>,
    /// Usage recorder for billing metering
    #[cfg(feature = "billing")]
    usage_recorder: Option<synapse_billing::UsageRecorder>,
    /// Billing client for pre-request credit checks
    #[cfg(feature = "billing")]
    billing_client: Option<synapse_billing::AetherClient>,
    /// Behavior when billing service is unreachable
    #[cfg(feature = "billing")]
    billing_fail_mode: FailMode,
    #[cfg(feature = "billing")]
    margins: synapse_core::MarginPolicy,
}

impl Server {
    /// Attach a usage recorder for billing metering
    ///
    /// Must be called before the server is shared with handlers.
    #[cfg(feature = "billing")]
    pub fn set_usage_recorder(&mut self, recorder: synapse_billing::UsageRecorder) {
        self.usage_recorder = Some(recorder);
    }

    /// Attach a billing client for pre-request credit checks
    ///
    /// Must be called before the server is shared with handlers.
    #[cfg(feature = "billing")]
    pub fn set_billing_client(
        &mut self,
        client: synapse_billing::AetherClient,
        fail_mode: FailMode,
        margins: synapse_core::MarginPolicy,
    ) {
        self.billing_client = Some(client);
        self.billing_fail_mode = fail_mode;
        self.margins = margins;
    }

    /// Amount to charge the request's billing identity for usage costing
    /// `base_usd` upstream, marked up for the billed product
    #[cfg(feature = "billing")]
    fn charge(&self, context: &RequestContext, provider_name: &str, base_usd: f64) -> f64 {
        context
            .billing_identity
            .as_ref()
            .map_or(0.0, |identity| self.margins.charge(identity, provider_name, base_usd))
    }

    /// Generate embeddings using the appropriate provider
    ///
    /// Routes to a provider based on the model name in the request.
    /// Model format: "provider/model" (e.g. "openai/text-embedding-3-small").
    /// If no provider prefix, uses the first configured provider.
    pub async fn embed(
        &self,
        request: &EmbeddingRequest,
        context: &RequestContext,
    ) -> crate::error::Result<EmbeddingResponse> {
        // Extract provider name from model (format: "provider/model")
        let (provider_name, _model_name) = request.model.split_once('/').unwrap_or(("", &request.model));

        let provider = if provider_name.is_empty() {
            // If no provider prefix, use the first provider
            self.providers
                .first()
                .ok_or_else(|| EmbeddingsError::ProviderNotFound("No embeddings providers configured".to_string()))?
        } else {
            self.providers
                .iter()
                .find(|p| p.name() == provider_name)
                .ok_or_else(|| EmbeddingsError::ProviderNotFound(provider_name.to_string()))?
        };

        // A BYOK request is not metered, so it must run on the caller's own
        // key, never the configured one
        let api_key = synapse_core::byok_provider_key(
            context.billing_identity.as_ref(),
            &context.provider_keys,
            provider.key_provider(),
        )
        .map_err(|e| EmbeddingsError::InvalidRequest(e.to_string()))?;

        // Pre-request spend check for managed billing
        #[cfg(feature = "billing")]
        {
            // Estimate tokens from input text length (~4 chars per token)
            let estimated_tokens: usize = request.input.as_vec().iter().map(|s| s.len() / 4).sum();
            #[allow(clippy::cast_precision_loss)]
            let estimated_cost = self.charge(context, provider.name(), estimated_tokens as f64 * 0.000_000_02); // ~$0.02/1M tokens
            if estimated_cost > 0.0 {
                self.check_spend(context, estimated_cost).await?;
            }
        }

        let response = provider.embed(request, context, api_key).await?;

        // Post-request billing: record usage and charge the actual cost
        #[cfg(feature = "billing")]
        {
            let pname = if provider_name.is_empty() {
                provider.name()
            } else {
                provider_name
            };
            let request_id = uuid::Uuid::new_v4().to_string();
            self.record_usage(
                context,
                pname,
                &request.model,
                response.usage.prompt_tokens,
                &request_id,
            );
            self.settle_spend(
                context,
                pname,
                &request.model,
                response.usage.prompt_tokens,
                &request_id,
            )
            .await;
        }

        Ok(response)
    }

    /// Check that the estimated cost is covered: by the credit wallet, or by
    /// the product's included allowance when it includes AI in its tier
    #[cfg(feature = "billing")]
    async fn check_spend(&self, context: &RequestContext, estimated_cost: f64) -> crate::error::Result<()> {
        let Some(ref client) = self.billing_client else {
            return Ok(());
        };

        let Some(ref identity) = context.billing_identity else {
            return Ok(());
        };

        if identity.mode != synapse_core::BillingMode::Managed {
            return Ok(());
        }

        match client
            .check_spend(
                &identity.app_id,
                &identity.entity_type,
                &identity.entity_id,
                estimated_cost,
            )
            .await
        {
            Ok(synapse_billing::SpendCheck::Allowed) => Ok(()),
            Ok(synapse_billing::SpendCheck::InsufficientCredits { balance }) => {
                Err(EmbeddingsError::InvalidRequest(format!(
                    "insufficient credits: estimated cost ${estimated_cost:.4} exceeds available balance ${balance:.4}"
                )))
            }
            Ok(synapse_billing::SpendCheck::AllowanceExhausted) => Err(EmbeddingsError::AllowanceExhausted),
            Err(e) => match self.billing_fail_mode {
                FailMode::Degraded => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, allowing embeddings request (degraded mode)"
                    );
                    Ok(())
                }
                FailMode::Open => {
                    tracing::warn!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, allowing embeddings request (fail-open mode)"
                    );
                    Ok(())
                }
                FailMode::Closed => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, rejecting embeddings request (fail-closed mode)"
                    );
                    Err(EmbeddingsError::BillingUnavailable)
                }
            },
        }
    }

    /// Record usage for billing metering
    #[cfg(feature = "billing")]
    fn record_usage(
        &self,
        context: &RequestContext,
        provider_name: &str,
        model_id: &str,
        prompt_tokens: u32,
        request_id: &str,
    ) {
        let Some(ref recorder) = self.usage_recorder else {
            return;
        };

        let Some(ref identity) = context.billing_identity else {
            return;
        };

        let is_byok = identity.mode == synapse_core::BillingMode::Byok;

        // For BYOK, zero out tokens; only the request count is metered
        recorder.record(synapse_billing::UsageEvent {
            app_id: identity.app_id.clone(),
            entity_type: identity.entity_type.clone(),
            entity_id: identity.entity_id.clone(),
            model: model_id.to_owned(),
            provider: provider_name.to_owned(),
            input_tokens: if is_byok { 0 } else { prompt_tokens },
            output_tokens: 0,
            estimated_cost_usd: 0.0,
            idempotency_key: request_id.to_owned(),
        });
    }

    /// Charge the actual cost after a successful embedding request to the credit wallet, or to the
    /// product's included allowance
    #[cfg(feature = "billing")]
    async fn settle_spend(
        &self,
        context: &RequestContext,
        provider_name: &str,
        model_id: &str,
        prompt_tokens: u32,
        request_id: &str,
    ) {
        let Some(ref client) = self.billing_client else {
            return;
        };

        let Some(ref identity) = context.billing_identity else {
            return;
        };

        if identity.mode != synapse_core::BillingMode::Managed {
            return;
        }

        // Cost based on token count (~$0.02/1M tokens for text-embedding-3-small)
        let actual_cost = self.charge(context, provider_name, f64::from(prompt_tokens) * 0.000_000_02);

        if actual_cost <= 0.0 {
            return;
        }

        let charge = synapse_billing::SpendCharge {
            amount_usd: actual_cost,
            description: format!("{provider_name}/{model_id}"),
            idempotency_key: request_id.to_owned(),
            reference_type: "embedding".to_owned(),
        };

        if let Err(e) = client
            .settle_spend(&identity.app_id, &identity.entity_type, &identity.entity_id, &charge)
            .await
        {
            tracing::warn!(
                error = %e,
                entity_id = %identity.entity_id,
                cost = actual_cost,
                "failed to charge usage after embedding"
            );
        }
    }
}

/// Builder for constructing the embeddings server from configuration
pub struct EmbeddingsServerBuilder<'a> {
    config: &'a synapse_config::Config,
}

impl<'a> EmbeddingsServerBuilder<'a> {
    pub fn new(config: &'a synapse_config::Config) -> Self {
        Self { config }
    }

    pub fn build(self) -> crate::error::Result<Server> {
        let mut providers: Vec<Box<dyn EmbeddingsProvider>> = Vec::new();

        for (name, provider_config) in &self.config.embeddings.providers {
            tracing::debug!("Initializing embeddings provider: {name}");

            let provider: Box<dyn EmbeddingsProvider> = match &provider_config.provider_type {
                EmbeddingsProviderType::Openai => {
                    let api_key = resolve_api_key(name, provider_config)?;

                    Box::new(OpenAiEmbeddingsProvider::new(
                        name.clone(),
                        api_key,
                        provider_config.base_url.clone(),
                    ))
                }
            };

            providers.push(provider);
        }

        if providers.is_empty() {
            tracing::debug!("No embeddings providers configured");
        } else {
            tracing::debug!("Embeddings server initialized with {} provider(s)", providers.len());
        }

        Ok(Server {
            providers,
            #[cfg(feature = "billing")]
            usage_recorder: None,
            #[cfg(feature = "billing")]
            billing_client: None,
            #[cfg(feature = "billing")]
            billing_fail_mode: FailMode::default(),
            #[cfg(feature = "billing")]
            margins: synapse_core::MarginPolicy::default(),
        })
    }
}

fn resolve_api_key(name: &str, config: &EmbeddingsProviderConfig) -> crate::error::Result<SecretString> {
    config
        .api_key
        .clone()
        .ok_or_else(|| EmbeddingsError::ConfigError(format!("API key required for embeddings provider '{name}'")))
}

#[cfg(test)]
mod key_tests {
    use secrecy::SecretString;
    use synapse_core::{BillingIdentity, BillingMode, RequestContext};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::Server;
    use crate::error::EmbeddingsError;
    use crate::provider::openai::OpenAiEmbeddingsProvider;
    use crate::types::EmbeddingRequest;

    fn server(provider: &MockServer) -> Server {
        named_server(provider, "openai")
    }

    fn named_server(provider: &MockServer, name: &str) -> Server {
        Server {
            providers: vec![Box::new(OpenAiEmbeddingsProvider::new(
                name.to_owned(),
                SecretString::from("configured-key".to_owned()),
                Some(format!("{}/v1", provider.uri())),
            ))],
            #[cfg(feature = "billing")]
            usage_recorder: None,
            #[cfg(feature = "billing")]
            billing_client: None,
            #[cfg(feature = "billing")]
            billing_fail_mode: synapse_config::FailMode::default(),
            #[cfg(feature = "billing")]
            margins: synapse_core::MarginPolicy::default(),
        }
    }

    async fn mount_provider(provider: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "object": "list",
                "data": [{ "object": "embedding", "embedding": [0.1], "index": 0 }],
                "model": "text-embedding-3-small",
                "usage": { "prompt_tokens": 1, "total_tokens": 1 }
            })))
            .mount(provider)
            .await;
    }

    fn request() -> EmbeddingRequest {
        serde_json::from_value(serde_json::json!({
            "input": "hello",
            "model": "openai/text-embedding-3-small"
        }))
        .unwrap()
    }

    fn context(mode: Option<BillingMode>, keys: &[(&str, &str)]) -> RequestContext {
        let mut ctx = RequestContext::empty();
        ctx.billing_identity = mode.map(|mode| BillingIdentity {
            app_id: "synapse".to_owned(),
            entity_type: "user".to_owned(),
            entity_id: "usr_1".to_owned(),
            mode,
            plan: None,
        });
        ctx.provider_keys = keys
            .iter()
            .map(|(name, key)| ((*name).to_owned(), SecretString::from(*key)))
            .collect();
        ctx
    }

    async fn authorization_sent(provider: &MockServer) -> Vec<String> {
        provider
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|r| {
                r.headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
            })
            .collect()
    }

    #[tokio::test]
    async fn byok_request_runs_on_the_callers_key() {
        let provider = MockServer::start().await;
        mount_provider(&provider).await;

        server(&provider)
            .embed(
                &request(),
                &context(Some(BillingMode::Byok), &[("openai", "byok-openai")]),
            )
            .await
            .unwrap();

        assert_eq!(authorization_sent(&provider).await, vec!["Bearer byok-openai"]);
    }

    #[tokio::test]
    async fn byok_request_on_a_custom_named_provider_runs_on_the_vendor_key() {
        // A provider configured under its own name still runs on the caller's
        // key for the vendor it calls
        let provider = MockServer::start().await;
        mount_provider(&provider).await;

        let request: EmbeddingRequest = serde_json::from_value(serde_json::json!({
            "input": "hello",
            "model": "embed/text-embedding-3-small"
        }))
        .unwrap();
        named_server(&provider, "embed")
            .embed(
                &request,
                &context(Some(BillingMode::Byok), &[("openai", "byok-openai")]),
            )
            .await
            .unwrap();

        assert_eq!(authorization_sent(&provider).await, vec!["Bearer byok-openai"]);
    }

    #[tokio::test]
    async fn byok_request_without_a_key_is_a_400_before_any_upstream_call() {
        let provider = MockServer::start().await;
        mount_provider(&provider).await;

        let err = server(&provider)
            .embed(
                &request(),
                &context(Some(BillingMode::Byok), &[("anthropic", "byok-other")]),
            )
            .await
            .unwrap_err();

        assert!(matches!(err, EmbeddingsError::InvalidRequest(_)), "{err:?}");
        assert_eq!(err.status_code(), axum::http::StatusCode::BAD_REQUEST);
        assert!(authorization_sent(&provider).await.is_empty());
    }

    #[tokio::test]
    async fn managed_request_runs_on_the_configured_key() {
        let provider = MockServer::start().await;
        mount_provider(&provider).await;

        server(&provider)
            .embed(
                &request(),
                &context(Some(BillingMode::Managed), &[("openai", "byok-ignored")]),
            )
            .await
            .unwrap();

        assert_eq!(authorization_sent(&provider).await, vec!["Bearer configured-key"]);
    }

    #[tokio::test]
    async fn request_without_a_billing_identity_runs_on_the_configured_key() {
        let provider = MockServer::start().await;
        mount_provider(&provider).await;

        server(&provider).embed(&request(), &context(None, &[])).await.unwrap();

        assert_eq!(authorization_sent(&provider).await, vec!["Bearer configured-key"]);
    }
}

#[cfg(all(test, feature = "billing"))]
mod spend_tests {
    use std::collections::HashMap;

    use secrecy::SecretString;
    use synapse_billing::{AetherClient, SpendPolicy};
    use synapse_config::FailMode;
    use synapse_core::{BillingIdentity, BillingMode, MarginPolicy, RequestContext};
    use wiremock::matchers::{method, path, path_regex, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::Server;
    use crate::error::EmbeddingsError;
    use crate::provider::openai::OpenAiEmbeddingsProvider;
    use crate::types::EmbeddingRequest;

    fn server(provider: &MockServer, aether: &MockServer) -> Server {
        let client = AetherClient::new(
            reqwest::Url::parse(&format!("{}/", aether.uri())).unwrap(),
            SecretString::from("aether-key".to_owned()),
        )
        .unwrap()
        .with_spend_policy(SpendPolicy::new(HashMap::from([(
            "myfi".to_owned(),
            "ai_credits".to_owned(),
        )])));

        Server {
            providers: vec![Box::new(OpenAiEmbeddingsProvider::new(
                "openai".to_owned(),
                SecretString::from("provider-key".to_owned()),
                Some(format!("{}/v1", provider.uri())),
            ))],
            usage_recorder: None,
            billing_client: Some(client),
            billing_fail_mode: FailMode::Degraded,
            margins: MarginPolicy::default(),
        }
    }

    fn myfi_context() -> RequestContext {
        let mut ctx = RequestContext::empty();
        ctx.billing_identity = Some(BillingIdentity {
            app_id: "myfi".to_owned(),
            entity_type: "organization".to_owned(),
            entity_id: "org_1".to_owned(),
            mode: BillingMode::Managed,
            plan: None,
        });
        ctx
    }

    // 4M chars estimate 1M tokens at $0.02/1M: $0.02 upstream, $0.04 retail
    // at the 2.0 default product margin, 4 credits
    fn request() -> EmbeddingRequest {
        serde_json::from_value(serde_json::json!({
            "input": "x".repeat(4_000_000),
            "model": "openai/text-embedding-3-small"
        }))
        .unwrap()
    }

    async fn forbid_wallet(aether: &MockServer) {
        Mock::given(path_regex(r"^/credits/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .named("an included-allowance product must never touch the credit wallet")
            .mount(aether)
            .await;
    }

    async fn mount_provider(provider: &MockServer, times: u64) {
        Mock::given(method("POST"))
            .and(path("/v1/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "object": "list",
                "data": [{ "object": "embedding", "embedding": [0.1], "index": 0 }],
                "model": "text-embedding-3-small",
                "usage": { "prompt_tokens": 1_000_000, "total_tokens": 1_000_000 }
            })))
            .expect(times)
            .mount(provider)
            .await;
    }

    #[tokio::test]
    async fn allowance_product_is_checked_and_charged_in_credits_never_the_wallet() {
        let (provider, aether) = (MockServer::start().await, MockServer::start().await);
        mount_provider(&provider, 1).await;
        forbid_wallet(&aether).await;
        Mock::given(method("GET"))
            .and(path("/usage/myfi/organization/org_1/ai_credits/check"))
            .and(query_param("additionalUsage", "4"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "allowed": true })))
            .expect(1)
            .mount(&aether)
            .await;
        Mock::given(method("POST"))
            .and(path("/usage/myfi/organization/org_1/ai_credits/record"))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({ "delta": 4.0 }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "eventId": "e" })))
            .expect(1)
            .mount(&aether)
            .await;

        server(&provider, &aether)
            .embed(&request(), &myfi_context())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn exhausted_allowance_is_refused_with_402_before_the_provider_is_called() {
        let (provider, aether) = (MockServer::start().await, MockServer::start().await);
        mount_provider(&provider, 0).await;
        forbid_wallet(&aether).await;
        Mock::given(method("GET"))
            .and(path("/usage/myfi/organization/org_1/ai_credits/check"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "allowed": false })))
            .expect(1)
            .mount(&aether)
            .await;

        let err = server(&provider, &aether)
            .embed(&request(), &myfi_context())
            .await
            .unwrap_err();

        assert!(matches!(err, EmbeddingsError::AllowanceExhausted));
        assert_eq!(err.status_code(), axum::http::StatusCode::PAYMENT_REQUIRED);
    }
}
