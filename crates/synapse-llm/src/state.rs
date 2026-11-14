//! Core LLM state and provider resolution logic

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use futures_util::{Stream, StreamExt};
use secrecy::SecretString;
use synapse_config::{FailMode, FailoverConfig, LlmConfig, LlmProviderType, RoutingConfig};
use synapse_core::RequestContext;
use synapse_routing::{FeedbackTracker, ModelRegistry, RequestFeedback, StrategyRegistry};

use crate::discovery;
use crate::error::LlmError;
use crate::health::ProviderHealthTracker;
use crate::provider::Provider;
use crate::routing::ModelRouter;
use crate::types::{CompletionRequest, CompletionResponse, StreamEvent};
use synapse_core::MarginPolicy;

/// Virtual model names that trigger smart routing
pub(crate) const ROUTING_CLASSES: &[&str] = &["auto", "fast", "best", "cheap"];

/// A stream of completion events
pub(crate) type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>;

/// The provider and model that served a request
///
/// Failover and cascade escalation can serve a request from a different model
/// than the one resolved for it, so billing, metering, usage reports, and the
/// cache follow this, never the resolved pair
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Served {
    pub(crate) provider: String,
    pub(crate) model: String,
}

impl Served {
    fn new(provider: &str, model: &str) -> Self {
        Self {
            provider: provider.to_owned(),
            model: model.to_owned(),
        }
    }
}

/// Shared state for LLM route handlers
#[derive(Clone)]
pub struct LlmState {
    pub(crate) inner: Arc<LlmStateInner>,
}

pub(crate) struct LlmStateInner {
    pub(crate) router: ModelRouter,
    pub(crate) providers: HashMap<String, Arc<dyn Provider>>,
    pub(crate) health: ProviderHealthTracker,
    pub(crate) failover: FailoverConfig,
    pub(crate) routing_config: RoutingConfig,
    pub(crate) model_registry: ModelRegistry,
    pub(crate) strategy_registry: StrategyRegistry,
    pub(crate) feedback: FeedbackTracker,
    /// Managed provider keys (provider name → API key) for managed billing mode
    pub(crate) managed_keys: HashMap<String, SecretString>,
    /// Markup applied to upstream cost for managed usage
    pub(crate) margins: MarginPolicy,
    /// Usage recorder for billing metering (billing feature only)
    #[cfg(feature = "billing")]
    pub(crate) usage_recorder: Option<synapse_billing::UsageRecorder>,
    /// Billing client for pre-request credit checks (billing feature only)
    #[cfg(feature = "billing")]
    pub(crate) billing_client: Option<synapse_billing::AetherClient>,
    /// Behavior when billing service is unreachable
    #[cfg(feature = "billing")]
    pub(crate) billing_fail_mode: FailMode,
    /// Response cache (cache feature only)
    #[cfg(feature = "cache")]
    pub(crate) response_cache: Option<synapse_cache::ResponseCache>,
}

impl LlmState {
    /// Execute a non-streaming completion with automatic provider
    /// resolution, smart routing, and failover
    ///
    /// # Errors
    ///
    /// Returns an error if model resolution or all provider attempts fail
    #[allow(clippy::cognitive_complexity)]
    pub async fn complete(
        &self,
        request: CompletionRequest,
        mut context: RequestContext,
    ) -> Result<CompletionResponse, LlmError> {
        let (provider_name, model_id, provider, explicit_provider) =
            self.resolve_provider(&request.model, &request, &context).await?;

        // Resolve API key based on billing mode
        self.resolve_api_key_for_request(&mut context, &provider_name)?;

        // Pre-request spend check for managed billing (the credit wallet, or
        // the product's included allowance), priced on the most expensive
        // model that may serve the request (a managed request for an unpriced
        // model is refused here)
        #[cfg(feature = "billing")]
        {
            let candidates = self.serving_candidates(&provider_name, &model_id, explicit_provider, false);
            if let Some(cost) = self.estimate_request_cost(&context, &candidates, &request)? {
                self.check_spend(&context, cost).await?;
            }
        }

        // The cache is consulted only after every check a live request gets
        // (auth and entitlements in middleware, provider and key resolution,
        // the spend check above), and a hit is billed exactly like a served
        // request: the caller receives the full value of the response, so the
        // cache is a gateway cost optimization, never a free tier
        #[cfg(feature = "cache")]
        let cache_key = self.response_cache_key(&request, &context);

        #[cfg(feature = "cache")]
        if let Some(ref key) = cache_key
            && let Some(ref cache) = self.inner.response_cache
        {
            match cache.get(key).await {
                Ok(Some(cached)) => match serde_json::from_str::<CompletionResponse>(&cached.body) {
                    Ok(response) => {
                        tracing::info!(
                            provider = %cached.provider,
                            model = %cached.model,
                            "serving cached response"
                        );
                        // Bill on the model that produced the cached response
                        self.record_served_usage(&context, &cached.provider, &cached.model, &response)
                            .await;
                        return Ok(response);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "cached response unreadable, proceeding without cache");
                    }
                },
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "cache lookup failed, proceeding without cache");
                }
            }
        }

        // Skip failover when the user explicitly selected a provider (e.g.
        // "nvidia/moonshotai/kimi-k2.5"), surface the error instead of
        // silently routing to a different model
        let (served, response) = if explicit_provider {
            self.complete_direct(&request, &context, &provider_name, &model_id, &provider)
                .await?
        } else {
            self.complete_with_failover(&request, &context, &provider_name, &model_id, &provider)
                .await?
        };

        self.record_served_usage(&context, &served.provider, &served.model, &response)
            .await;

        // Store successful response in cache. A response without usage is
        // never stored, since a hit on it could not be billed
        #[cfg(feature = "cache")]
        if let Some(ref key) = cache_key
            && response.usage.is_some()
            && let Some(ref cache) = self.inner.response_cache
            && let Ok(body) = serde_json::to_string(&response)
        {
            let entry = synapse_cache::CachedResponse {
                body,
                model: served.model,
                provider: served.provider,
            };
            if let Err(e) = cache.put(key, &entry, None).await {
                tracing::warn!(error = %e, "failed to cache response");
            }
        }

        Ok(response)
    }

    /// Meter, charge, and report a served completion's usage
    ///
    /// Shared by live responses and cache hits so both are billed the same
    /// way: usage events, the managed spend settlement (credit wallet or
    /// included allowance, each with a fresh idempotency key), and the
    /// dashboard usage report
    async fn record_served_usage(
        &self,
        context: &RequestContext,
        provider_name: &str,
        model_id: &str,
        response: &CompletionResponse,
    ) {
        let Some(ref usage) = response.usage else {
            return;
        };

        // Record usage for billing
        #[cfg(feature = "billing")]
        if let Some(ref recorder) = self.inner.usage_recorder {
            dispatch_usage_event(
                recorder,
                context,
                provider_name,
                model_id,
                usage.prompt_tokens,
                usage.completion_tokens,
                &self.inner.model_registry,
                &self.inner.margins,
            );
        }

        // Post-completion charge for actual usage
        #[cfg(feature = "billing")]
        self.settle_spend_for_usage(
            context,
            provider_name,
            model_id,
            usage.prompt_tokens,
            usage.completion_tokens,
        )
        .await;

        // Report usage to synapse-api for dashboard charts
        dispatch_usage_report(
            context,
            provider_name,
            model_id,
            usage.prompt_tokens,
            usage.completion_tokens,
            &self.inner.model_registry,
            &self.inner.margins,
        );
    }

    /// Cache key for a request, when the response cache applies to it
    ///
    /// `None` unless the cache is configured, the caller opted in, the
    /// request is not streamed, and the caller has an identity to scope the
    /// entry to (an anonymous request is never cached)
    #[cfg(feature = "cache")]
    fn response_cache_key(
        &self,
        request: &CompletionRequest,
        context: &RequestContext,
    ) -> Option<synapse_cache::CacheKey> {
        self.inner.response_cache.as_ref()?;

        let opted_in = synapse_cache::opt_in_requested(
            context
                .headers()
                .get(synapse_cache::OPT_IN_HEADER)
                .and_then(|v| v.to_str().ok()),
        );
        if !synapse_cache::is_cacheable(opted_in, request.stream) {
            return None;
        }

        let scope = cache_scope(context)?;
        synapse_cache::CacheKey::new(&scope, request)
            .inspect_err(|e| tracing::warn!(error = %e, "cache key derivation failed, proceeding without cache"))
            .ok()
    }

    /// Execute a streaming completion with automatic provider
    /// resolution, smart routing, failover, and optional cascade
    ///
    /// # Errors
    ///
    /// Returns an error if model resolution or all provider attempts fail
    #[allow(clippy::too_many_lines)]
    pub async fn complete_stream(
        &self,
        request: CompletionRequest,
        mut context: RequestContext,
    ) -> Result<(String, EventStream), LlmError> {
        let original_model = request.model.clone();
        let (provider_name, model_id, provider, explicit_provider) =
            self.resolve_provider(&request.model, &request, &context).await?;

        // Resolve API key based on billing mode
        self.resolve_api_key_for_request(&mut context, &provider_name)?;

        let cascade = !explicit_provider && self.is_cascade_strategy(&original_model);

        // Pre-request spend check for managed billing (the credit wallet, or
        // the product's included allowance), priced on the most expensive
        // model that may serve the request (a managed request for an unpriced
        // model is refused here)
        #[cfg(feature = "billing")]
        {
            let candidates = self.serving_candidates(&provider_name, &model_id, explicit_provider, cascade);
            if let Some(cost) = self.estimate_request_cost(&context, &candidates, &request)? {
                self.check_spend(&context, cost).await?;
            }
        }

        // Skip failover/cascade when the user explicitly selected a
        // provider, surface the error instead of silently routing to a
        // different model
        let (served, stream) = if explicit_provider {
            self.complete_stream_direct(&request, &context, &provider_name, &model_id, &provider)
                .await?
        } else if cascade {
            self.complete_stream_with_cascade(
                &request,
                &context,
                &provider_name,
                &model_id,
                &provider,
                &self.inner.routing_config.cascade,
            )
            .await?
        } else {
            self.complete_stream_with_failover(&request, &context, &provider_name, &model_id, &provider)
                .await?
        };

        // Wrap stream to intercept usage events for billing and reporting,
        // all on the model that served the stream
        let actual_model = served.model.clone();
        let usage_reporter = context.parts.extensions.get::<synapse_auth::UsageReporter>().cloned();
        let resolved_key = synapse_auth::resolved_key(&context.parts.extensions).cloned();

        #[cfg(feature = "billing")]
        if let Some(ref recorder) = self.inner.usage_recorder {
            let recorder = recorder.clone();
            let inner = Arc::clone(&self.inner);
            let ctx = context.clone();
            let Served {
                provider: prov,
                model: mdl,
            } = served;
            let reporter = usage_reporter;
            let resolved = resolved_key;

            let billing_client = inner.billing_client.clone();
            let metered_stream = stream.map(move |item| {
                if let Ok(StreamEvent::Usage(ref usage)) = item {
                    dispatch_usage_event(
                        &recorder,
                        &ctx,
                        &prov,
                        &mdl,
                        usage.prompt_tokens,
                        usage.completion_tokens,
                        &inner.model_registry,
                        &inner.margins,
                    );

                    // Charge actual usage (fire-and-forget)
                    if let Some(ref client) = billing_client {
                        spawn_spend_settlement(
                            client.clone(),
                            &ctx,
                            &prov,
                            &mdl,
                            usage.prompt_tokens,
                            usage.completion_tokens,
                            &inner.model_registry,
                            &inner.margins,
                        );
                    }

                    // Report usage to synapse-api for dashboard charts
                    if let Some(ref reporter) = reporter
                        && let Some(ref resolved) = resolved
                    {
                        record_usage_report(
                            reporter,
                            resolved,
                            &prov,
                            &mdl,
                            usage.prompt_tokens,
                            usage.completion_tokens,
                            &inner.model_registry,
                            &inner.margins,
                        );
                    }
                }
                item
            });

            return Ok((actual_model, Box::pin(metered_stream)));
        }

        // Non-billing path: still report usage for dashboard charts
        if let (Some(reporter), Some(resolved)) = (usage_reporter, resolved_key) {
            let inner = Arc::clone(&self.inner);
            let Served {
                provider: prov,
                model: mdl,
            } = served;

            let reporting_stream = stream.map(move |item| {
                if let Ok(StreamEvent::Usage(ref usage)) = item {
                    record_usage_report(
                        &reporter,
                        &resolved,
                        &prov,
                        &mdl,
                        usage.prompt_tokens,
                        usage.completion_tokens,
                        &inner.model_registry,
                        &inner.margins,
                    );
                }
                item
            });

            return Ok((actual_model, Box::pin(reporting_stream)));
        }

        Ok((actual_model, stream))
    }

    /// Build `LlmState` from configuration, constructing all providers
    ///
    /// # Errors
    ///
    /// Returns an error if any provider fails to initialize.
    pub async fn from_config(config: LlmConfig) -> Result<Self, LlmError> {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();

        for (name, provider_config) in &config.providers {
            if provider_config.forward_authorization {
                tracing::warn!(
                    provider = %name,
                    "forward_authorization is deprecated and has no effect, the resolved request key is always used"
                );
            }

            let provider: Arc<dyn Provider> = match &provider_config.provider_type {
                LlmProviderType::Openai => Arc::new(crate::provider::openai::OpenAiProvider::new(
                    name.clone(),
                    provider_config,
                )?),
                LlmProviderType::Anthropic => Arc::new(crate::provider::anthropic::AnthropicProvider::new(
                    name.clone(),
                    provider_config,
                )?),
                LlmProviderType::Google => Arc::new(crate::provider::google::GoogleProvider::new(
                    name.clone(),
                    provider_config,
                )?),
                LlmProviderType::Bedrock(_) => {
                    Arc::new(crate::provider::bedrock::BedrockProvider::new(name.clone(), provider_config).await?)
                }
            };

            providers.insert(name.clone(), provider);
        }

        let health = ProviderHealthTracker::new(config.failover.circuit_breaker.clone());
        let failover = config.failover.clone();
        let routing_config = config.routing.clone();
        let model_registry = ModelRegistry::from_config(&config.routing.models);
        let strategy_registry = StrategyRegistry::from_config(&config.routing);
        let router = ModelRouter::new(&config);
        let feedback = FeedbackTracker::new();

        // Start background model discovery
        discovery::start_discovery(config, router.known_models());

        Ok(Self {
            inner: Arc::new(LlmStateInner {
                router,
                providers,
                health,
                failover,
                routing_config,
                model_registry,
                strategy_registry,
                feedback,
                managed_keys: HashMap::new(),
                margins: MarginPolicy::default(),
                #[cfg(feature = "billing")]
                usage_recorder: None,
                #[cfg(feature = "billing")]
                billing_client: None,
                #[cfg(feature = "billing")]
                billing_fail_mode: FailMode::default(),
                #[cfg(feature = "cache")]
                response_cache: None,
            }),
        })
    }

    /// Configure managed provider keys for billing
    ///
    /// Must be called before the state is shared with handlers.
    ///
    /// # Panics
    ///
    /// Panics if called after the inner `Arc` has been cloned
    pub fn set_managed_keys(&mut self, keys: HashMap<String, SecretString>) {
        let inner = Arc::get_mut(&mut self.inner).expect("set_managed_keys must be called before state is shared");
        inner.managed_keys = keys;
    }

    /// Configure the markup applied to managed usage
    ///
    /// Must be called before the state is shared with handlers.
    ///
    /// # Panics
    ///
    /// Panics if called after the inner `Arc` has been cloned
    pub fn set_margin_policy(&mut self, margins: MarginPolicy) {
        let inner = Arc::get_mut(&mut self.inner).expect("set_margin_policy must be called before state is shared");
        inner.margins = margins;
    }

    /// Attach a usage recorder for billing metering
    ///
    /// Must be called before the state is shared with handlers.
    ///
    /// # Panics
    ///
    /// Panics if called after the inner `Arc` has been cloned (i.e.
    /// after the state has been shared with route handlers)
    #[cfg(feature = "billing")]
    pub fn set_usage_recorder(&mut self, recorder: synapse_billing::UsageRecorder) {
        Arc::get_mut(&mut self.inner)
            .expect("set_usage_recorder must be called before state is shared")
            .usage_recorder = Some(recorder);
    }

    /// Attach a billing client for pre-request credit checks
    ///
    /// Must be called before the state is shared with handlers.
    ///
    /// # Panics
    ///
    /// Panics if called after the inner `Arc` has been cloned
    #[cfg(feature = "billing")]
    pub fn set_billing_client(&mut self, client: synapse_billing::AetherClient, fail_mode: FailMode) {
        let inner = Arc::get_mut(&mut self.inner).expect("set_billing_client must be called before state is shared");
        inner.billing_client = Some(client);
        inner.billing_fail_mode = fail_mode;
    }

    /// Attach a response cache for LLM completions
    ///
    /// Must be called before the state is shared with handlers.
    ///
    /// # Panics
    ///
    /// Panics if called after the inner `Arc` has been cloned
    #[cfg(feature = "cache")]
    pub fn set_response_cache(&mut self, cache: synapse_cache::ResponseCache) {
        Arc::get_mut(&mut self.inner)
            .expect("set_response_cache must be called before state is shared")
            .response_cache = Some(cache);
    }

    /// List all available models across providers
    pub async fn list_models(&self) -> Vec<(String, String)> {
        self.inner.router.list_models().await
    }

    /// Resolve a model name and get the corresponding provider
    ///
    /// Handles both normal model names and virtual routing classes
    /// ("auto", "fast", "best", "cheap") when smart routing is enabled.
    pub(crate) async fn resolve_provider(
        &self,
        model: &str,
        request: &CompletionRequest,
        context: &RequestContext,
    ) -> Result<(String, String, Arc<dyn Provider>, bool), LlmError> {
        // Check for virtual routing classes, gated on smart_routing entitlement
        if self.inner.routing_config.enabled && ROUTING_CLASSES.contains(&model) {
            let entitled = self.check_smart_routing_entitlement(context).await;
            if entitled {
                let (pn, mi, p) = self.resolve_via_routing(model, request)?;
                return Ok((pn, mi, p, false));
            }
            tracing::debug!("smart routing not entitled, falling back to requested model");
        }

        let resolved = self.inner.router.resolve(model).await?;
        let provider = self
            .inner
            .providers
            .get(&resolved.provider_name)
            .ok_or_else(|| LlmError::ProviderNotFound {
                provider: resolved.provider_name.clone(),
            })?;
        Ok((
            resolved.provider_name.clone(),
            resolved.model_id,
            Arc::clone(provider),
            resolved.explicit_provider,
        ))
    }

    /// Resolve a virtual model name via the smart routing system
    fn resolve_via_routing(
        &self,
        routing_class: &str,
        request: &CompletionRequest,
    ) -> Result<(String, String, Arc<dyn Provider>), LlmError> {
        use crate::types::message::{Content, ContentPart, Role};

        // Convert internal messages to JSON values for analysis
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": serde_json::to_value(&m.role).unwrap_or_default(),
                    "content": m.content.as_text()
                })
            })
            .collect();

        let has_tools = request.tools.as_ref().is_some_and(|t| !t.is_empty());

        // Extract conversation signals
        let message_count = request.messages.len();

        let has_system_prompt = request.messages.iter().any(|m| m.role == Role::System);

        let has_images = request.messages.iter().any(|m| {
            matches!(&m.content, Content::Parts(parts) if parts.iter().any(|p| matches!(p, ContentPart::Image { .. })))
        });

        let tool_call_turns = request
            .messages
            .iter()
            .filter(|m| m.role == Role::Assistant && m.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty()))
            .count();

        let user_message_count = request.messages.iter().filter(|m| m.role == Role::User).count();
        let is_multi_turn = user_message_count > 1;

        // Apply routing class overrides
        let config = self.map_routing_class(routing_class);

        let input = synapse_routing::AnalysisInput {
            messages: &messages,
            has_tools,
            has_images,
            message_count,
            has_system_prompt,
            tool_call_turns,
            is_multi_turn,
        };

        let decision = synapse_routing::route_with_strategy_registry(
            &input,
            &self.inner.model_registry,
            &config,
            &self.inner.strategy_registry,
            Some(&self.inner.feedback),
        )
        .map_err(|e| LlmError::InvalidRequest(format!("routing failed: {e}")))?;

        let provider = self
            .inner
            .providers
            .get(&decision.provider)
            .ok_or_else(|| LlmError::ProviderNotFound {
                provider: decision.provider.clone(),
            })?;

        tracing::info!(
            routing_class,
            provider = %decision.provider,
            model = %decision.model,
            reason = ?decision.reason,
            "smart routing resolved virtual model"
        );

        Ok((decision.provider, decision.model, Arc::clone(provider)))
    }

    /// Map a routing class name to an appropriate routing config
    fn map_routing_class(&self, class: &str) -> RoutingConfig {
        let mut config = self.inner.routing_config.clone();

        match class {
            "fast" | "cheap" => {
                // Force cost strategy for cheap/fast
                config.strategy = synapse_config::RoutingStrategy::Cost;
            }
            "best" => {
                // Force threshold strategy biased toward high quality
                config.strategy = synapse_config::RoutingStrategy::Threshold;
                config.threshold.quality_floor = 0.9;
            }
            // "auto" uses whatever strategy is configured
            _ => {}
        }

        config
    }

    /// Check whether the current user is entitled to smart routing
    ///
    /// Returns `true` when billing is disabled (no entitlement gate) or
    /// when the Aether `smart_routing` entitlement grants access.
    /// Falls back to `false` on errors or missing identity.
    async fn check_smart_routing_entitlement(&self, context: &RequestContext) -> bool {
        #[cfg(feature = "billing")]
        {
            let Some(ref client) = self.inner.billing_client else {
                // No billing client configured, no entitlement gate
                return true;
            };
            let Some(ref identity) = context.billing_identity else {
                // No billing identity, allow routing (auth middleware
                // already permitted the request)
                return true;
            };

            match client
                .check_entitlement(
                    &identity.app_id,
                    &identity.entity_type,
                    &identity.entity_id,
                    "smart_routing",
                )
                .await
            {
                Ok(resp) => resp.has_access,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to check smart_routing entitlement, denying");
                    false
                }
            }
        }

        #[cfg(not(feature = "billing"))]
        {
            let _ = context;
            true
        }
    }

    /// Execute a non-streaming completion without failover
    ///
    /// Used when the user explicitly selected a provider/model; errors
    /// surface directly instead of silently routing to a different model
    pub(crate) async fn complete_direct(
        &self,
        request: &CompletionRequest,
        context: &RequestContext,
        provider_name: &str,
        model_id: &str,
        provider: &Arc<dyn Provider>,
    ) -> Result<(Served, CompletionResponse), LlmError> {
        let mut req = request.clone();
        model_id.clone_into(&mut req.model);

        let start = Instant::now();
        match provider.complete(&req, context).await {
            Ok(response) => {
                self.inner.health.record_success(provider_name);
                self.inner.feedback.record(&RequestFeedback {
                    provider: provider_name.to_owned(),
                    model: model_id.to_owned(),
                    latency: start.elapsed(),
                    success: true,
                    input_tokens: response.usage.as_ref().map(|u| u.prompt_tokens),
                    output_tokens: response.usage.as_ref().map(|u| u.completion_tokens),
                });
                Ok((Served::new(provider_name, model_id), response))
            }
            Err(e) => {
                self.inner.health.record_failure(provider_name);
                self.inner.feedback.record(&RequestFeedback {
                    provider: provider_name.to_owned(),
                    model: model_id.to_owned(),
                    latency: start.elapsed(),
                    success: false,
                    input_tokens: None,
                    output_tokens: None,
                });
                tracing::warn!(
                    provider = provider_name,
                    model = model_id,
                    error = %e,
                    "explicit provider failed, not failing over"
                );
                Err(e)
            }
        }
    }

    /// Execute a streaming completion without failover
    ///
    /// Used when the user explicitly selected a provider/model
    pub(crate) async fn complete_stream_direct(
        &self,
        request: &CompletionRequest,
        context: &RequestContext,
        provider_name: &str,
        model_id: &str,
        provider: &Arc<dyn Provider>,
    ) -> Result<(Served, EventStream), LlmError> {
        let mut req = request.clone();
        model_id.clone_into(&mut req.model);

        let start = Instant::now();
        match provider.complete_stream(&req, context).await {
            Ok(stream) => {
                self.inner.health.record_success(provider_name);
                self.inner.feedback.record(&RequestFeedback {
                    provider: provider_name.to_owned(),
                    model: model_id.to_owned(),
                    latency: start.elapsed(),
                    success: true,
                    input_tokens: None,
                    output_tokens: None,
                });
                Ok((Served::new(provider_name, model_id), stream))
            }
            Err(e) => {
                self.inner.health.record_failure(provider_name);
                self.inner.feedback.record(&RequestFeedback {
                    provider: provider_name.to_owned(),
                    model: model_id.to_owned(),
                    latency: start.elapsed(),
                    success: false,
                    input_tokens: None,
                    output_tokens: None,
                });
                tracing::warn!(
                    provider = provider_name,
                    model = model_id,
                    error = %e,
                    "explicit provider streaming failed, not failing over"
                );
                Err(e)
            }
        }
    }

    /// Execute a non-streaming completion with failover support
    #[allow(clippy::cognitive_complexity)]
    pub(crate) async fn complete_with_failover(
        &self,
        request: &CompletionRequest,
        context: &RequestContext,
        provider_name: &str,
        model_id: &str,
        provider: &Arc<dyn Provider>,
    ) -> Result<(Served, CompletionResponse), LlmError> {
        // Try primary provider
        let mut req = request.clone();
        model_id.clone_into(&mut req.model);

        let start = Instant::now();
        match provider.complete(&req, context).await {
            Ok(response) => {
                self.inner.health.record_success(provider_name);
                self.inner.feedback.record(&RequestFeedback {
                    provider: provider_name.to_owned(),
                    model: model_id.to_owned(),
                    latency: start.elapsed(),
                    success: true,
                    input_tokens: response.usage.as_ref().map(|u| u.prompt_tokens),
                    output_tokens: response.usage.as_ref().map(|u| u.completion_tokens),
                });
                Ok((Served::new(provider_name, model_id), response))
            }
            Err(e) => {
                self.inner.health.record_failure(provider_name);
                self.inner.feedback.record(&RequestFeedback {
                    provider: provider_name.to_owned(),
                    model: model_id.to_owned(),
                    latency: start.elapsed(),
                    success: false,
                    input_tokens: None,
                    output_tokens: None,
                });

                if !self.inner.failover.enabled || !e.is_retryable() {
                    return Err(e);
                }

                tracing::warn!(
                    provider = provider_name,
                    model = model_id,
                    error = %e,
                    "primary provider failed, attempting failover"
                );

                let mut last_error = e;

                for (alt_provider, alt_model) in self.failover_alternatives(provider_name, model_id) {
                    if !self.inner.health.is_available(&alt_provider) {
                        tracing::debug!(
                            provider = %alt_provider,
                            "skipping unhealthy provider"
                        );
                        continue;
                    }

                    if !self.can_bill(context, &alt_provider, &alt_model) {
                        tracing::debug!(
                            provider = %alt_provider,
                            model = %alt_model,
                            "skipping unpriced alternative for a managed request"
                        );
                        continue;
                    }

                    let Some(alt_provider_impl) = self.inner.providers.get(&alt_provider) else {
                        continue;
                    };

                    let alt_context = match self.context_for_provider(context, &alt_provider) {
                        Ok(keyed) => keyed,
                        Err(e) => {
                            tracing::debug!(
                                provider = %alt_provider,
                                error = %e,
                                "skipping alternative provider without a usable key"
                            );
                            continue;
                        }
                    };

                    tracing::warn!(
                        from_provider = provider_name,
                        to_provider = %alt_provider,
                        to_model = %alt_model,
                        "failing over to alternative provider"
                    );

                    let mut alt_req = request.clone();
                    alt_req.model.clone_from(&alt_model);

                    match alt_provider_impl.complete(&alt_req, &alt_context).await {
                        Ok(response) => {
                            self.inner.health.record_success(&alt_provider);
                            return Ok((Served::new(&alt_provider, &alt_model), response));
                        }
                        Err(e) => {
                            self.inner.health.record_failure(&alt_provider);
                            tracing::warn!(
                                provider = %alt_provider,
                                error = %e,
                                "failover provider also failed"
                            );
                            last_error = e;
                        }
                    }
                }

                Err(last_error)
            }
        }
    }

    /// Execute a streaming completion with failover support
    ///
    /// Failover is only possible before streaming starts. If the initial
    /// `complete_stream()` call returns an error, alternatives are tried.
    pub(crate) async fn complete_stream_with_failover(
        &self,
        request: &CompletionRequest,
        context: &RequestContext,
        provider_name: &str,
        model_id: &str,
        provider: &Arc<dyn Provider>,
    ) -> Result<(Served, EventStream), LlmError> {
        // Try primary provider
        let mut req = request.clone();
        model_id.clone_into(&mut req.model);

        let start = Instant::now();
        match provider.complete_stream(&req, context).await {
            Ok(stream) => {
                self.inner.health.record_success(provider_name);
                // Record success feedback for stream initiation
                self.inner.feedback.record(&RequestFeedback {
                    provider: provider_name.to_owned(),
                    model: model_id.to_owned(),
                    latency: start.elapsed(),
                    success: true,
                    input_tokens: None,
                    output_tokens: None,
                });
                Ok((Served::new(provider_name, model_id), stream))
            }
            Err(e) => {
                self.inner.health.record_failure(provider_name);
                self.inner.feedback.record(&RequestFeedback {
                    provider: provider_name.to_owned(),
                    model: model_id.to_owned(),
                    latency: start.elapsed(),
                    success: false,
                    input_tokens: None,
                    output_tokens: None,
                });

                if !self.inner.failover.enabled || !e.is_retryable() {
                    return Err(e);
                }

                tracing::warn!(
                    provider = provider_name,
                    model = model_id,
                    error = %e,
                    "primary provider streaming failed, attempting failover"
                );

                let mut last_error = e;

                for (alt_provider, alt_model) in self.failover_alternatives(provider_name, model_id) {
                    if !self.inner.health.is_available(&alt_provider)
                        || !self.can_bill(context, &alt_provider, &alt_model)
                    {
                        continue;
                    }

                    let Some(alt_provider_impl) = self.inner.providers.get(&alt_provider) else {
                        continue;
                    };

                    let alt_context = match self.context_for_provider(context, &alt_provider) {
                        Ok(keyed) => keyed,
                        Err(e) => {
                            tracing::debug!(
                                provider = %alt_provider,
                                error = %e,
                                "skipping alternative provider without a usable key"
                            );
                            continue;
                        }
                    };

                    tracing::warn!(
                        from_provider = provider_name,
                        to_provider = %alt_provider,
                        to_model = %alt_model,
                        "failing over streaming to alternative provider"
                    );

                    let mut alt_req = request.clone();
                    alt_req.model.clone_from(&alt_model);

                    match alt_provider_impl.complete_stream(&alt_req, &alt_context).await {
                        Ok(stream) => {
                            self.inner.health.record_success(&alt_provider);
                            return Ok((Served::new(&alt_provider, &alt_model), stream));
                        }
                        Err(e) => {
                            self.inner.health.record_failure(&alt_provider);
                            last_error = e;
                        }
                    }
                }

                Err(last_error)
            }
        }
    }

    /// Execute a streaming cascade: buffer initial model's response, evaluate
    /// confidence, then either replay the buffer or re-request with the
    /// escalation model
    #[allow(clippy::cognitive_complexity)]
    pub(crate) async fn complete_stream_with_cascade(
        &self,
        request: &CompletionRequest,
        context: &RequestContext,
        provider_name: &str,
        model_id: &str,
        provider: &Arc<dyn Provider>,
        cascade_config: &synapse_config::CascadeConfig,
    ) -> Result<(Served, EventStream), LlmError> {
        // Get the escalation model from the routing decision's alternatives
        let escalation = self.resolve_escalation_model(cascade_config)?;

        // Stream from the initial (cheap) model
        let (initial, mut stream) = self
            .complete_stream_with_failover(request, context, provider_name, model_id, provider)
            .await?;

        // Buffer stream events, collecting text content
        let mut buffered_events: Vec<StreamEvent> = Vec::new();
        let mut buffered_text = String::new();
        let mut buffer_bytes: usize = 0;
        let mut committed = false;

        let timeout = tokio::time::Duration::from_secs(cascade_config.buffer_timeout_secs);
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let next = tokio::time::timeout_at(deadline, stream.next()).await;

            match next {
                // Timeout fired, commit to initial model
                Err(_elapsed) => {
                    tracing::debug!(
                        model = %initial.model,
                        buffered_bytes = buffer_bytes,
                        "cascade buffer timeout, committing to initial model"
                    );
                    committed = true;
                    break;
                }
                // Stream ended
                Ok(None) => break,
                // Stream error, propagate
                Ok(Some(Err(e))) => return Err(e),
                // Stream event
                Ok(Some(Ok(event))) => {
                    // Track buffer size
                    if let StreamEvent::Delta(ref delta) = event
                        && let Some(ref content) = delta.content
                    {
                        buffer_bytes += content.len();
                        buffered_text.push_str(content);
                    }

                    buffered_events.push(event.clone());

                    // If buffer limit exceeded, commit
                    if buffer_bytes >= cascade_config.max_buffer_bytes {
                        tracing::debug!(
                            model = %initial.model,
                            buffered_bytes = buffer_bytes,
                            "cascade buffer limit exceeded, committing to initial model"
                        );
                        committed = true;
                        break;
                    }

                    // Done event means stream completed
                    if matches!(event, StreamEvent::Done) {
                        break;
                    }
                }
            }
        }

        // If committed early (buffer limit or timeout), replay buffer + remaining stream
        if committed {
            return Ok((initial, Box::pin(replay(buffered_events).chain(stream))));
        }

        // Estimate input tokens for confidence check
        let query_tokens: usize = request.messages.iter().map(|m| m.content.as_text().len() / 4).sum();

        // Evaluate confidence on buffered response
        let is_confident = synapse_routing::strategy::cascade::evaluate_buffered_response(
            &buffered_text,
            query_tokens,
            cascade_config.confidence_threshold,
        );

        if is_confident {
            tracing::debug!(
                model = %initial.model,
                "cascade: initial model response is confident, replaying buffer"
            );
            return Ok((initial, Box::pin(replay(buffered_events))));
        }

        // Not confident, escalate to stronger model
        tracing::info!(
            initial_model = %initial.model,
            escalation_provider = %escalation.0,
            escalation_model = %escalation.1,
            "cascade: escalating to stronger model"
        );

        let (esc_provider_name, esc_model_id) = &escalation;

        // A managed request is never escalated to a model it cannot be billed for
        if !self.can_bill(context, esc_provider_name, esc_model_id) {
            tracing::info!(
                escalation_provider = %esc_provider_name,
                escalation_model = %esc_model_id,
                "cascade: escalation model is unpriced for a managed request, replaying buffer"
            );
            return Ok((initial, Box::pin(replay(buffered_events))));
        }

        let esc_provider = self
            .inner
            .providers
            .get(esc_provider_name)
            .ok_or_else(|| LlmError::ProviderNotFound {
                provider: esc_provider_name.clone(),
            })?;

        // The escalation provider needs its own key; without a usable one,
        // keep the initial model's response rather than send it another key
        let esc_context = match self.context_for_provider(context, esc_provider_name) {
            Ok(keyed) => keyed,
            Err(e) => {
                tracing::info!(
                    escalation_provider = %esc_provider_name,
                    error = %e,
                    "cascade: no usable key for the escalation provider, replaying buffer"
                );
                return Ok((initial, Box::pin(replay(buffered_events))));
            }
        };

        self.complete_stream_with_failover(request, &esc_context, esc_provider_name, esc_model_id, esc_provider)
            .await
    }

    /// Resolve the escalation model for cascade routing
    fn resolve_escalation_model(
        &self,
        cascade_config: &synapse_config::CascadeConfig,
    ) -> Result<(String, String), LlmError> {
        if let Some(ref configured) = cascade_config.escalation_model {
            let (provider, model) = configured
                .split_once('/')
                .ok_or_else(|| LlmError::InvalidRequest("invalid escalation model format".to_owned()))?;
            return Ok((provider.to_owned(), model.to_owned()));
        }

        // Default to best quality model in registry
        let best = self
            .inner
            .model_registry
            .best_quality()
            .ok_or_else(|| LlmError::InvalidRequest("no escalation model available".to_owned()))?;
        Ok((best.provider.clone(), best.model.clone()))
    }

    /// Resolve the upstream key for a request to `provider_name` based on
    /// billing mode, overwriting any key resolved for another provider
    ///
    /// - BYOK: the caller's own key for the provider. A BYOK request is not
    ///   metered, so it must never fall back to the configured key
    /// - Managed: the managed key for the provider, or `None` (the provider's
    ///   configured key) when none is set
    /// - No billing identity: `None`, so the provider uses its configured key
    ///
    /// # Errors
    ///
    /// Returns `LlmError::InvalidRequest` when a BYOK caller has no key
    /// registered for the provider, or the provider cannot carry a caller key
    fn resolve_api_key_for_request(&self, context: &mut RequestContext, provider_name: &str) -> Result<(), LlmError> {
        use synapse_core::BillingMode;

        context.api_key = match context.billing_identity.as_ref().map(|identity| &identity.mode) {
            None => None,
            Some(BillingMode::Byok) => {
                if self
                    .inner
                    .providers
                    .get(provider_name)
                    .is_some_and(|provider| !provider.accepts_request_api_key())
                {
                    return Err(LlmError::InvalidRequest(format!(
                        "provider '{provider_name}' does not support BYOK keys. Use a managed-mode API key"
                    )));
                }
                let key = synapse_core::byok_provider_key(
                    context.billing_identity.as_ref(),
                    &context.provider_keys,
                    provider_name,
                )
                .map_err(|e| LlmError::InvalidRequest(e.to_string()))?;
                key.cloned()
            }
            Some(BillingMode::Managed) => self.inner.managed_keys.get(provider_name).cloned(),
        };

        Ok(())
    }

    /// A copy of `context` keyed for `provider_name`
    ///
    /// Failover and cascade escalation reach providers other than the one the
    /// request was keyed for, so each attempt is keyed again: a key is only
    /// ever sent to the provider it belongs to
    fn context_for_provider(&self, context: &RequestContext, provider_name: &str) -> Result<RequestContext, LlmError> {
        let mut keyed = context.clone();
        self.resolve_api_key_for_request(&mut keyed, provider_name)?;
        Ok(keyed)
    }

    /// The alternatives failover may try once `provider_name`/`model_id`
    /// fails, in order (`max_attempts` includes the primary)
    fn failover_alternatives(&self, provider_name: &str, model_id: &str) -> Vec<(String, String)> {
        if !self.inner.failover.enabled {
            return Vec::new();
        }
        let remaining = self.inner.failover.max_attempts.saturating_sub(1);
        ModelRouter::find_equivalents(provider_name, model_id, &self.inner.failover.equivalence_groups)
            .into_iter()
            .take(remaining)
            .collect()
    }

    /// Every provider/model pair that may serve a request: the resolved pair
    /// first, then the failover alternatives it can reach, and for a cascade
    /// the escalation model and its alternatives
    #[cfg(feature = "billing")]
    fn serving_candidates(
        &self,
        provider_name: &str,
        model_id: &str,
        explicit_provider: bool,
        cascade: bool,
    ) -> Vec<(String, String)> {
        let mut candidates = vec![(provider_name.to_owned(), model_id.to_owned())];
        if explicit_provider {
            return candidates;
        }
        candidates.extend(self.failover_alternatives(provider_name, model_id));
        if cascade
            && let Ok((esc_provider, esc_model)) = self.resolve_escalation_model(&self.inner.routing_config.cascade)
        {
            candidates.extend(self.failover_alternatives(&esc_provider, &esc_model));
            candidates.push((esc_provider, esc_model));
        }
        candidates
    }

    /// Whether a request served by `provider_name`/`model_id` can be billed
    ///
    /// Always for an unmetered request. A managed request served by a model
    /// with no price profile would bill nothing, so failover and escalation
    /// never route a managed request to one
    #[cfg_attr(not(feature = "billing"), allow(clippy::unused_self, clippy::missing_const_for_fn))]
    fn can_bill(&self, context: &RequestContext, provider_name: &str, model_id: &str) -> bool {
        #[cfg(feature = "billing")]
        {
            let managed = context
                .billing_identity
                .as_ref()
                .is_some_and(|identity| identity.mode == synapse_core::BillingMode::Managed);
            !managed || self.inner.model_registry.find(provider_name, model_id).is_some()
        }

        #[cfg(not(feature = "billing"))]
        {
            let _ = (context, provider_name, model_id);
            true
        }
    }

    /// Check if the current routing strategy is cascade
    pub(crate) fn is_cascade_strategy(&self, model: &str) -> bool {
        if !self.inner.routing_config.enabled || !ROUTING_CLASSES.contains(&model) {
            return false;
        }
        let config = self.map_routing_class(model);
        matches!(config.strategy, synapse_config::RoutingStrategy::Cascade)
    }
}

/// Replay buffered stream events
fn replay(events: Vec<StreamEvent>) -> impl Stream<Item = Result<StreamEvent, LlmError>> + Send {
    futures_util::stream::iter(events.into_iter().map(Ok))
}

/// The tenant a cached response belongs to
///
/// The billing identity (product, entity, managed vs BYOK) plus the API key
/// that made the request, falling back to the resolved key's owner, then the
/// client identity when billing is off. `None` when the request carries no
/// identity at all, so it is never cached
#[cfg(feature = "cache")]
fn cache_scope(context: &RequestContext) -> Option<synapse_cache::CacheScope> {
    use synapse_cache::{CacheBillingMode, CacheScope};

    let resolved = synapse_auth::resolved_key(&context.parts.extensions);
    let principal = resolved.map(|r| format!("{}:{}", r.user_id, r.api_key_id));

    if let Some(identity) = context.billing_identity.as_ref() {
        let mode = match identity.mode {
            synapse_core::BillingMode::Managed => CacheBillingMode::Managed,
            synapse_core::BillingMode::Byok => CacheBillingMode::Byok,
        };
        return CacheScope::new(
            identity.app_id.as_str(),
            identity.entity_type.as_str(),
            identity.entity_id.as_str(),
            mode,
            principal,
        );
    }

    if let Some(resolved) = resolved {
        let mode = match resolved.mode {
            synapse_auth::KeyMode::Byok => CacheBillingMode::Byok,
            synapse_auth::KeyMode::Managed | synapse_auth::KeyMode::Manual => CacheBillingMode::Managed,
        };
        return CacheScope::new(
            resolved.app_id.clone().unwrap_or_default(),
            "user",
            resolved.user_id.as_str(),
            mode,
            principal,
        );
    }

    let client = context.client_identity.as_ref()?;
    CacheScope::new(
        "",
        "client",
        client.client_id.as_str(),
        CacheBillingMode::Unbilled,
        None,
    )
}

/// Price a managed request on the model that will actually serve it
///
/// `model_id` is the resolved, bare model id the registry is keyed by, never
/// the raw request id (`anthropic/...` or a virtual model like `auto`), which
/// never matches and used to price every such request at $0. A model with no
/// price profile is refused: serving it on Omni's key would bill nothing
#[cfg(feature = "billing")]
fn managed_cost(
    registry: &ModelRegistry,
    provider_name: &str,
    model_id: &str,
    input_tokens: usize,
    output_tokens: usize,
    margin: f64,
) -> Result<f64, LlmError> {
    registry
        .find(provider_name, model_id)
        .map(|profile| profile.estimate_cost(input_tokens, output_tokens) * margin)
        .ok_or_else(|| LlmError::ModelNotPriced {
            model: format!("{provider_name}/{model_id}"),
        })
}

/// Estimate the cost of a request based on input tokens
///
/// Returns `Ok(None)` if the user is not in managed billing mode, and an
/// error if a managed request names a model the gateway cannot price
#[cfg(feature = "billing")]
impl LlmState {
    fn estimate_request_cost(
        &self,
        context: &RequestContext,
        candidates: &[(String, String)],
        request: &CompletionRequest,
    ) -> Result<Option<f64>, LlmError> {
        let Some(identity) = context.billing_identity.as_ref() else {
            return Ok(None);
        };
        if identity.mode != synapse_core::BillingMode::Managed {
            return Ok(None);
        }

        // Estimate input tokens from message content
        let estimated_input: usize = request.messages.iter().map(|m| m.content.as_text().len() / 4).sum();

        // Estimate output tokens conservatively (use max_tokens if set, else default)
        let estimated_output = request.params.max_tokens.unwrap_or(1024) as usize;

        let cost = |provider_name: &str, model_id: &str| {
            let margin = self
                .inner
                .margins
                .resolve(Some(&identity.app_id), identity.plan.as_deref(), provider_name);
            managed_cost(
                &self.inner.model_registry,
                provider_name,
                model_id,
                estimated_input,
                estimated_output,
                margin,
            )
        };

        // The resolved model must be priced. An unpriced alternative is never
        // served to a managed request (see `can_bill`), so it is left out
        let Some(((provider_name, model_id), alternatives)) = candidates.split_first() else {
            return Ok(None);
        };
        let primary = cost(provider_name, model_id)?;
        let worst = alternatives
            .iter()
            .filter_map(|(provider_name, model_id)| cost(provider_name, model_id).ok())
            .fold(primary, f64::max);

        Ok(Some(worst))
    }

    /// Check that the estimated cost is covered: by the credit wallet, or by
    /// the product's included allowance when it includes AI in its tier
    async fn check_spend(&self, context: &RequestContext, estimated_cost: f64) -> Result<(), LlmError> {
        let Some(ref client) = self.inner.billing_client else {
            return Ok(());
        };

        let Some(ref identity) = context.billing_identity else {
            return Ok(());
        };

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
            Ok(synapse_billing::SpendCheck::InsufficientCredits { balance }) => Err(LlmError::InsufficientCredits {
                message: format!("estimated cost ${estimated_cost:.4} exceeds available balance ${balance:.4}"),
            }),
            Ok(synapse_billing::SpendCheck::AllowanceExhausted) => Err(LlmError::AllowanceExhausted),
            Err(e) => match self.inner.billing_fail_mode {
                FailMode::Degraded => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, allowing request under emergency token cap (degraded mode)"
                    );
                    Ok(())
                }
                FailMode::Open => {
                    tracing::warn!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, allowing request (fail-open mode)"
                    );
                    Ok(())
                }
                FailMode::Closed => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "spend check failed, rejecting request (fail-closed mode)"
                    );
                    Err(LlmError::BillingUnavailable)
                }
            },
        }
    }

    /// Charge a successful completion's actual usage to the credit wallet, or
    /// to the product's included allowance
    async fn settle_spend_for_usage(
        &self,
        context: &RequestContext,
        provider_name: &str,
        model_id: &str,
        input_tokens: u32,
        output_tokens: u32,
    ) {
        let Some(ref client) = self.inner.billing_client else {
            return;
        };

        let Some(ref identity) = context.billing_identity else {
            return;
        };

        if identity.mode != synapse_core::BillingMode::Managed {
            return;
        }

        let plan = identity.plan.as_deref();
        let actual_cost = self
            .inner
            .model_registry
            .find(provider_name, model_id)
            .map_or(0.0, |profile| {
                let base = profile.estimate_cost(input_tokens as usize, output_tokens as usize);
                let margin = self.inner.margins.resolve(Some(&identity.app_id), plan, provider_name);
                base * margin
            });

        if actual_cost <= 0.0 {
            return;
        }

        let charge = synapse_billing::SpendCharge {
            amount_usd: actual_cost,
            description: format!("{provider_name}/{model_id}"),
            idempotency_key: uuid::Uuid::new_v4().to_string(),
            reference_type: "completion".to_owned(),
        };

        if let Err(e) = client
            .settle_spend(&identity.app_id, &identity.entity_type, &identity.entity_id, &charge)
            .await
        {
            tracing::warn!(
                error = %e,
                entity_id = %identity.entity_id,
                cost = actual_cost,
                "failed to charge usage after completion"
            );
        }
    }
}

/// Spawn a fire-and-forget charge of a streamed completion's actual usage to
/// the credit wallet, or to the product's included allowance
#[cfg(feature = "billing")]
#[allow(clippy::too_many_arguments)]
fn spawn_spend_settlement(
    client: synapse_billing::AetherClient,
    context: &RequestContext,
    provider_name: &str,
    model_id: &str,
    input_tokens: u32,
    output_tokens: u32,
    model_registry: &ModelRegistry,
    margins: &MarginPolicy,
) {
    let Some(ref identity) = context.billing_identity else {
        return;
    };

    if identity.mode != synapse_core::BillingMode::Managed {
        return;
    }

    let plan = identity.plan.as_deref();
    let actual_cost = model_registry.find(provider_name, model_id).map_or(0.0, |profile| {
        let base = profile.estimate_cost(input_tokens as usize, output_tokens as usize);
        let margin = margins.resolve(Some(&identity.app_id), plan, provider_name);
        base * margin
    });

    if actual_cost <= 0.0 {
        return;
    }

    let app_id = identity.app_id.clone();
    let entity_type = identity.entity_type.clone();
    let entity_id = identity.entity_id.clone();
    let desc = format!("{provider_name}/{model_id}");

    tokio::spawn(async move {
        let charge = synapse_billing::SpendCharge {
            amount_usd: actual_cost,
            description: desc,
            idempotency_key: uuid::Uuid::new_v4().to_string(),
            reference_type: "completion".to_owned(),
        };

        if let Err(e) = client.settle_spend(&app_id, &entity_type, &entity_id, &charge).await {
            tracing::warn!(
                error = %e,
                entity_id = %entity_id,
                cost = actual_cost,
                "failed to charge usage after streaming completion"
            );
        }
    });
}

/// Dispatch a usage event to the billing recorder
#[cfg(feature = "billing")]
#[allow(clippy::too_many_arguments)]
fn dispatch_usage_event(
    recorder: &synapse_billing::UsageRecorder,
    context: &RequestContext,
    provider_name: &str,
    model_id: &str,
    input_tokens: u32,
    output_tokens: u32,
    model_registry: &ModelRegistry,
    margins: &MarginPolicy,
) {
    let Some(ref identity) = context.billing_identity else {
        return;
    };

    let is_byok = identity.mode == synapse_core::BillingMode::Byok;

    // For BYOK, zero out tokens and cost; only the request count is metered
    // (the recorder always records delta=1 for the requests meter)
    let (metered_input, metered_output) = if is_byok { (0, 0) } else { (input_tokens, output_tokens) };

    let estimated_cost_usd = if is_byok {
        0.0
    } else {
        let plan = identity.plan.as_deref();
        model_registry.find(provider_name, model_id).map_or(0.0, |profile| {
            let base = profile.estimate_cost(input_tokens as usize, output_tokens as usize);
            let margin = margins.resolve(Some(&identity.app_id), plan, provider_name);
            base * margin
        })
    };

    let idempotency_key = uuid::Uuid::new_v4().to_string();

    recorder.record(synapse_billing::UsageEvent {
        app_id: identity.app_id.clone(),
        entity_type: identity.entity_type.clone(),
        entity_id: identity.entity_id.clone(),
        model: model_id.to_owned(),
        provider: provider_name.to_owned(),
        input_tokens: metered_input,
        output_tokens: metered_output,
        estimated_cost_usd,
        idempotency_key,
    });
}

/// Dispatch a usage event to the synapse-api usage reporter for dashboard charts
#[allow(clippy::too_many_arguments)]
fn dispatch_usage_report(
    context: &RequestContext,
    provider_name: &str,
    model_id: &str,
    input_tokens: u32,
    output_tokens: u32,
    model_registry: &ModelRegistry,
    margins: &MarginPolicy,
) {
    let Some(reporter) = context.parts.extensions.get::<synapse_auth::UsageReporter>() else {
        return;
    };

    let Some(resolved) = synapse_auth::resolved_key(&context.parts.extensions) else {
        return;
    };

    record_usage_report(
        reporter,
        resolved,
        provider_name,
        model_id,
        input_tokens,
        output_tokens,
        model_registry,
        margins,
    );
}

/// Build and record a usage event to synapse-api
#[allow(clippy::too_many_arguments)]
fn record_usage_report(
    reporter: &synapse_auth::UsageReporter,
    resolved: &synapse_auth::ResolvedKey,
    provider_name: &str,
    model_id: &str,
    input_tokens: u32,
    output_tokens: u32,
    model_registry: &ModelRegistry,
    margins: &MarginPolicy,
) {
    let estimated_cost_usd = model_registry.find(provider_name, model_id).map_or(0.0, |profile| {
        let base = profile.estimate_cost(input_tokens as usize, output_tokens as usize);
        let margin = margins.resolve(resolved.app_id.as_deref(), Some(&resolved.plan), provider_name);
        base * margin
    });
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let cost_cents = if estimated_cost_usd.is_finite() && estimated_cost_usd >= 0.0 {
        (estimated_cost_usd * 100.0).round() as u32
    } else {
        0
    };

    let mode = match resolved.mode {
        synapse_auth::KeyMode::Byok => "byok",
        synapse_auth::KeyMode::Managed | synapse_auth::KeyMode::Manual => "managed",
    };

    reporter.record(synapse_auth::UsageEvent {
        user_id: resolved.user_id.clone(),
        workspace_id: resolved.workspace_id.clone(),
        api_key_id: resolved.api_key_id.clone(),
        provider: provider_name.to_owned(),
        model: model_id.to_owned(),
        input_tokens,
        output_tokens,
        cost_cents,
        mode: mode.to_owned(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verify the public API surface compiles
    #[test]
    fn complete_method_exists() {
        // Type-level check: ensure `complete` method signature is correct
        fn _assert_complete(state: &LlmState, req: CompletionRequest, ctx: RequestContext) {
            let _fut = state.complete(req, ctx);
        }
    }

    #[test]
    fn complete_stream_method_exists() {
        fn _assert_stream(state: &LlmState, req: CompletionRequest, ctx: RequestContext) {
            let _fut = state.complete_stream(req, ctx);
        }
    }

    #[cfg(feature = "billing")]
    fn priced_registry() -> ModelRegistry {
        ModelRegistry::from_config(&[synapse_config::ModelProfileConfig {
            provider: "anthropic".to_owned(),
            model: "claude-sonnet-4-5-20250929".to_owned(),
            context_window: 200_000,
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            quality: 0.9,
            capabilities: synapse_config::ModelCapabilities::default(),
            long_context_pricing: None,
        }])
    }

    #[cfg(feature = "billing")]
    #[test]
    fn managed_cost_prices_the_resolved_model_with_margin() {
        let registry = priced_registry();

        // 1M input at $3 + 1M output at $15, times a 2.0 margin
        let cost = managed_cost(
            &registry,
            "anthropic",
            "claude-sonnet-4-5-20250929",
            1_000_000,
            1_000_000,
            2.0,
        );

        assert!(cost.is_ok_and(|c| (c - 36.0).abs() < 1e-9));
    }

    #[cfg(feature = "billing")]
    #[test]
    fn managed_cost_rejects_an_unpriced_model() {
        // Serving a model the gateway cannot price would bill it at $0, so a
        // managed request for one must be refused before reaching the provider
        let registry = priced_registry();

        let cost = managed_cost(&registry, "anthropic", "claude-opus-5-5", 1_000, 1_000, 2.0);

        assert!(matches!(cost, Err(LlmError::ModelNotPriced { .. })));
    }

    #[cfg(feature = "billing")]
    #[test]
    fn managed_cost_does_not_match_a_provider_prefixed_id() {
        // The registry keys bare model ids; callers must pass the resolved id,
        // never the raw request id (`anthropic/...`), which used to price at $0
        let registry = priced_registry();

        let cost = managed_cost(
            &registry,
            "anthropic",
            "anthropic/claude-sonnet-4-5-20250929",
            1_000,
            1_000,
            1.0,
        );

        assert!(cost.is_err());
    }

    #[test]
    fn model_not_priced_is_a_client_error_and_never_retried() {
        use synapse_core::HttpError;

        let err = LlmError::ModelNotPriced {
            model: "anthropic/claude-opus-5-5".to_owned(),
        };

        assert_eq!(err.status_code(), http::StatusCode::BAD_REQUEST);
        assert!(!err.is_retryable());
        assert!(err.client_message().contains("bring your own"));
    }
}
