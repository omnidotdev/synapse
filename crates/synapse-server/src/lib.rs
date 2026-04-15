mod auth;
mod billing_identity;
mod client_id;
mod cors;
mod csrf;
mod entitlement;
mod entitlement_cache;
mod guardrails;
mod health;
mod invalidate;
mod rate_limit;
mod request_context;
mod webhook;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use synapse_config::Config;
use synapse_llm::LlmState;
use synapse_mcp::McpState;
use tower_http::trace::TraceLayer;

/// Assembled server with all routes and middleware
pub struct Server {
    router: Router,
    listen_address: SocketAddr,
}

impl Server {
    /// Build the server from configuration
    ///
    /// # Errors
    ///
    /// Returns an error if subsystem initialization (LLM, MCP, STT, TTS) or
    /// rate-limiter construction fails
    ///
    /// # Panics
    ///
    /// Panics if billing is configured but subsystem `Arc` states have already
    /// been shared (this should never happen during initialization)
    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        let listen_address = config
            .server
            .listen_address
            .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 3000)));

        if config.server.tls.is_some() {
            tracing::warn!("TLS configuration is present but application-level TLS is not supported; use a reverse proxy or ingress controller for TLS termination");
        }

        // Initialize subsystems that borrow config before LLM consumes it
        let mut stt_state = stt::build_server(&config)?;
        let mut tts_state = tts::build_server(&config)?;
        let mut embeddings_state = synapse_embeddings::build_server(&config)?;
        let mut imagegen_state = synapse_imagegen::build_server(&config)?;
        let mut llm_state = LlmState::from_config(config.llm).await?;

        // Configure billing for all modalities when enabled
        if let Some(ref billing_config) = config.billing
            && billing_config.enabled
        {
            // Set managed provider keys for LLM
            use secrecy::SecretString;
            use std::collections::HashMap;

            let mut managed_keys: HashMap<String, SecretString> = HashMap::new();
            let mut managed_margins: HashMap<String, f64> = HashMap::new();
            for (name, provider_config) in &billing_config.managed_providers {
                managed_keys.insert(name.clone(), provider_config.api_key.clone());
                managed_margins.insert(name.clone(), provider_config.margin);
            }
            llm_state.set_managed_providers(managed_keys, managed_margins);
            llm_state.set_tier_margins(billing_config.tier_margins.clone());

            // Attach usage recorder for LLM
            let recorder_client = synapse_billing::AetherClient::new(
                billing_config.billing_url.clone(),
                billing_config.app_id.clone(),
                billing_config.service_api_key.clone(),
            )?;
            let meter_keys = synapse_billing::MeterKeys {
                input_tokens: billing_config.meters.input_tokens.clone(),
                output_tokens: billing_config.meters.output_tokens.clone(),
                requests: billing_config.meters.requests.clone(),
                messages: billing_config.meters.messages.clone(),
            };
            let recorder = synapse_billing::UsageRecorder::new(recorder_client, meter_keys);
            llm_state.set_usage_recorder(recorder);

            // Attach billing client for pre-request credit checks (LLM)
            let credit_client = synapse_billing::AetherClient::new(
                billing_config.billing_url.clone(),
                billing_config.app_id.clone(),
                billing_config.service_api_key.clone(),
            )?;
            llm_state.set_billing_client(credit_client, billing_config.fail_mode.clone());

            // Wire billing into STT
            {
                let client = synapse_billing::AetherClient::new(
                    billing_config.billing_url.clone(),
                    billing_config.app_id.clone(),
                    billing_config.service_api_key.clone(),
                )?;
                let recorder_client = synapse_billing::AetherClient::new(
                    billing_config.billing_url.clone(),
                    billing_config.app_id.clone(),
                    billing_config.service_api_key.clone(),
                )?;
                let meter_keys = synapse_billing::MeterKeys {
                    input_tokens: billing_config.meters.input_tokens.clone(),
                    output_tokens: billing_config.meters.output_tokens.clone(),
                    requests: billing_config.meters.requests.clone(),
                    messages: billing_config.meters.messages.clone(),
                };
                let recorder = synapse_billing::UsageRecorder::new(recorder_client, meter_keys);
                let stt = Arc::get_mut(&mut stt_state).expect("STT state not yet shared");
                stt.set_billing_client(client, billing_config.fail_mode.clone());
                stt.set_usage_recorder(recorder);
            }

            // Wire billing into TTS
            {
                let client = synapse_billing::AetherClient::new(
                    billing_config.billing_url.clone(),
                    billing_config.app_id.clone(),
                    billing_config.service_api_key.clone(),
                )?;
                let recorder_client = synapse_billing::AetherClient::new(
                    billing_config.billing_url.clone(),
                    billing_config.app_id.clone(),
                    billing_config.service_api_key.clone(),
                )?;
                let meter_keys = synapse_billing::MeterKeys {
                    input_tokens: billing_config.meters.input_tokens.clone(),
                    output_tokens: billing_config.meters.output_tokens.clone(),
                    requests: billing_config.meters.requests.clone(),
                    messages: billing_config.meters.messages.clone(),
                };
                let recorder = synapse_billing::UsageRecorder::new(recorder_client, meter_keys);
                let tts = Arc::get_mut(&mut tts_state).expect("TTS state not yet shared");
                tts.set_billing_client(client, billing_config.fail_mode.clone());
                tts.set_usage_recorder(recorder);
            }

            // Wire billing into embeddings
            {
                let client = synapse_billing::AetherClient::new(
                    billing_config.billing_url.clone(),
                    billing_config.app_id.clone(),
                    billing_config.service_api_key.clone(),
                )?;
                let recorder_client = synapse_billing::AetherClient::new(
                    billing_config.billing_url.clone(),
                    billing_config.app_id.clone(),
                    billing_config.service_api_key.clone(),
                )?;
                let meter_keys = synapse_billing::MeterKeys {
                    input_tokens: billing_config.meters.input_tokens.clone(),
                    output_tokens: billing_config.meters.output_tokens.clone(),
                    requests: billing_config.meters.requests.clone(),
                    messages: billing_config.meters.messages.clone(),
                };
                let recorder = synapse_billing::UsageRecorder::new(recorder_client, meter_keys);
                let emb = Arc::get_mut(&mut embeddings_state).expect("embeddings state not yet shared");
                emb.set_billing_client(client, billing_config.fail_mode.clone());
                emb.set_usage_recorder(recorder);
            }

            // Wire billing into image generation
            {
                let client = synapse_billing::AetherClient::new(
                    billing_config.billing_url.clone(),
                    billing_config.app_id.clone(),
                    billing_config.service_api_key.clone(),
                )?;
                let recorder_client = synapse_billing::AetherClient::new(
                    billing_config.billing_url.clone(),
                    billing_config.app_id.clone(),
                    billing_config.service_api_key.clone(),
                )?;
                let meter_keys = synapse_billing::MeterKeys {
                    input_tokens: billing_config.meters.input_tokens.clone(),
                    output_tokens: billing_config.meters.output_tokens.clone(),
                    requests: billing_config.meters.requests.clone(),
                    messages: billing_config.meters.messages.clone(),
                };
                let recorder = synapse_billing::UsageRecorder::new(recorder_client, meter_keys);
                let img = Arc::get_mut(&mut imagegen_state).expect("imagegen state not yet shared");
                img.set_billing_client(client, billing_config.fail_mode.clone());
                img.set_usage_recorder(recorder);
            }
        }

        // Configure response cache when enabled
        if let Some(ref cache_config) = config.cache
            && cache_config.enabled
        {
            let cache = synapse_cache::ResponseCache::new(
                cache_config.url.as_str(),
                std::time::Duration::from_secs(cache_config.ttl_seconds),
                Some(cache_config.key_prefix.clone()),
            )
            .map_err(|e| anyhow::anyhow!("failed to initialize response cache: {e}"))?;
            llm_state.set_response_cache(cache);
            tracing::info!(ttl_seconds = cache_config.ttl_seconds, "response cache enabled");
        }

        let mcp_state = Arc::new(McpState::new(&config.mcp).await?);

        // Build base router with feature routes
        let mut app = Router::new();

        // Health check
        if config.server.health.enabled {
            app = app.route(&config.server.health.path, axum::routing::get(health::health_handler));
        }

        // LLM routes
        app = app.merge(synapse_llm::llm_router(llm_state));

        // MCP routes
        app = app.merge(synapse_mcp::mcp_router(mcp_state));

        // STT routes
        app = app.merge(stt::endpoint_router().with_state(stt_state));

        // TTS routes
        app = app.merge(tts::endpoint_router().with_state(tts_state));

        // Embeddings routes
        app = app.merge(synapse_embeddings::endpoint_router().with_state(embeddings_state));

        // Image generation routes
        app = app.merge(synapse_imagegen::endpoint_router().with_state(imagegen_state));

        // Apply middleware layers (innermost first)

        // Content guardrails (runs just before handlers, after all auth/rate limiting)
        if let Some(ref guardrails_config) = config.guardrails
            && guardrails_config.enabled
        {
            let engine = synapse_guardrails::GuardrailEngine::new(&guardrails_config.rules)
                .map_err(|e| anyhow::anyhow!("failed to compile guardrail rules: {e}"))?;
            let engine = Arc::new(engine);
            if !engine.is_empty() {
                app = app.layer(axum::middleware::from_fn(move |req, next| {
                    let engine = Arc::clone(&engine);
                    async move { guardrails::guardrails_middleware(engine, req, next).await }
                }));
                tracing::info!(rules = guardrails_config.rules.len(), "guardrails enabled");
            }
        }

        // Request context (innermost; collects auth/identity data, runs just before handlers)
        app = app.layer(axum::middleware::from_fn(request_context::request_context_middleware));

        // Tracing
        app = app.layer(TraceLayer::new_for_http());

        // CORS
        if let Some(ref cors_config) = config.server.cors {
            app = app.layer(cors::cors_layer(cors_config));
        }

        // CSRF protection
        if let Some(ref csrf_config) = config.server.csrf
            && csrf_config.enabled
        {
            let header_name = csrf_config.header_name.clone();
            app = app.layer(axum::middleware::from_fn(move |req, next| {
                let header_name = header_name.clone();
                async move { csrf::csrf_middleware(header_name, req, next).await }
            }));
        }

        // Client identification
        if let Some(ref client_id_config) = config.server.client_identification {
            let cid_config = client_id_config.clone();
            app = app.layer(axum::middleware::from_fn(move |req, next| {
                let config = cid_config.clone();
                async move { client_id::client_id_middleware(config, req, next).await }
            }));
        }

        // Rate limiting
        if let Some(ref rl_config) = config.server.rate_limit {
            let limiter = Arc::new(synapse_ratelimit::create_request_limiter(rl_config)?);
            app = app.layer(axum::middleware::from_fn(move |req, next| {
                let limiter = Arc::clone(&limiter);
                async move { rate_limit::rate_limit_middleware_arc(limiter, req, next).await }
            }));
        }

        // Entitlement gate (inner to auth, so auth runs first and sets BillingIdentity)
        if let Some(ref billing_config) = config.billing
            && billing_config.enabled
        {
            let aether_client = synapse_billing::AetherClient::new(
                billing_config.billing_url.clone(),
                billing_config.app_id.clone(),
                billing_config.service_api_key.clone(),
            )?;
            let ent_state = entitlement::EntitlementState::new(aether_client, billing_config.clone());

            // Entitlement webhook endpoint (shares cache with middleware)
            if let Some(ref auth_config) = config.auth {
                let webhook_state = webhook::WebhookState {
                    cache: ent_state.cache(),
                    gateway_secret: auth_config.gateway_secret.clone(),
                    billing_config: billing_config.clone(),
                };
                app = app.route(
                    "/webhooks/entitlements",
                    axum::routing::post(webhook::entitlement_webhook_handler).with_state(webhook_state),
                );
            }

            app = app.layer(axum::middleware::from_fn(move |req, next| {
                let state = ent_state.clone();
                async move { entitlement::entitlement_middleware(state, req, next).await }
            }));
        }

        // Billing identity from JWT, fallback for when auth middleware does not set BillingIdentity
        // (e.g. JWT-authenticated requests that bypass API key auth)
        if let Some(ref billing_config) = config.billing
            && billing_config.enabled
        {
            let bc = billing_config.clone();
            app = app.layer(axum::middleware::from_fn(move |req, next| {
                let config = bc.clone();
                async move { billing_identity::billing_identity_middleware(config, req, next).await }
            }));
        }

        // API key authentication (outermost; processes all requests first and sets BillingIdentity)
        if let Some(ref auth_config) = config.auth
            && auth_config.enabled
        {
            let resolver = synapse_auth::ApiKeyResolver::new(
                auth_config.api_url.clone(),
                auth_config.gateway_secret.clone(),
                std::time::Duration::from_secs(auth_config.cache_ttl_seconds),
                auth_config.cache_capacity,
                auth_config.tls_skip_verify,
            )?;

            // Optionally construct VaultClient for BYOK key resolution from Gatekeeper
            let vault_client: Option<Arc<synapse_auth::VaultClient>> = if let Some(ref vault_config) = auth_config.vault
            {
                let client = synapse_auth::VaultClient::new(
                    vault_config.url.clone(),
                    vault_config.service_key.clone(),
                    Some(std::time::Duration::from_secs(vault_config.cache_ttl_seconds)),
                    Some(vault_config.cache_capacity),
                )?;
                tracing::info!(
                    url = %vault_config.url,
                    cache_ttl_secs = vault_config.cache_ttl_seconds,
                    "vault client enabled for BYOK key resolution"
                );
                Some(Arc::new(client))
            } else {
                None
            };

            // Cache invalidation endpoint
            let invalidate_state = invalidate::InvalidateState {
                resolver: resolver.clone(),
                gateway_secret: auth_config.gateway_secret.clone(),
            };
            app = app.route(
                "/internal/invalidate-key",
                axum::routing::post(invalidate::invalidate_key_handler).with_state(invalidate_state),
            );

            let usage_reporter = synapse_auth::UsageReporter::spawn(
                auth_config.api_url.clone(),
                auth_config.gateway_secret.clone(),
                std::time::Duration::from_secs(10),
            );

            let public_paths = auth_config.public_paths.clone();
            let reporter = Some(usage_reporter);
            app = app.layer(axum::middleware::from_fn(move |req, next| {
                let resolver = resolver.clone();
                let vault = vault_client.clone();
                let public_paths = public_paths.clone();
                let reporter = reporter.clone();
                async move { auth::auth_middleware(resolver, vault, public_paths, reporter, req, next).await }
            }));
        }

        Ok(Self {
            router: app,
            listen_address,
        })
    }

    /// Get the configured listen address
    #[must_use]
    pub const fn listen_address(&self) -> SocketAddr {
        self.listen_address
    }

    /// Consume the server and return the inner router
    ///
    /// Useful for testing when the caller manages the listener
    pub fn into_router(self) -> Router {
        self.router
    }

    /// Start serving requests
    ///
    /// Blocks until the cancellation token is triggered.
    ///
    /// # Errors
    ///
    /// Returns an error if binding the TCP listener or serving fails
    pub async fn serve(self, shutdown: tokio_util::sync::CancellationToken) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind(self.listen_address).await?;
        let local_addr = listener.local_addr()?;
        tracing::info!(%local_addr, "server listening");

        axum::serve(listener, self.router)
            .with_graceful_shutdown(async move {
                shutdown.cancelled().await;
                tracing::info!("graceful shutdown initiated");
            })
            .await?;

        Ok(())
    }
}
