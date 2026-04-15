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

/// Virtual model names that trigger smart routing
pub(crate) const ROUTING_CLASSES: &[&str] = &["auto", "fast", "best", "cheap"];

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
    /// Managed provider margins (provider name → margin multiplier)
    pub(crate) managed_margins: HashMap<String, f64>,
    /// Per-tier margin multipliers (plan name → margin multiplier)
    pub(crate) tier_margins: HashMap<String, f64>,
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
        // Check response cache for deterministic requests
        #[cfg(feature = "cache")]
        let cache_key = if synapse_cache::is_cacheable(request.params.temperature, request.stream) {
            if let Some(ref cache) = self.inner.response_cache {
                let key = synapse_cache::compute_cache_key(&request);
                match cache.get(&key).await {
                    Ok(Some(cached)) => {
                        tracing::info!("serving cached response");
                        let response: CompletionResponse = serde_json::from_str(&cached.body)
                            .map_err(|e| LlmError::Internal(anyhow::anyhow!("cache deserialization: {e}")))?;
                        return Ok(response);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "cache lookup failed, proceeding without cache");
                    }
                }
                Some(key)
            } else {
                None
            }
        } else {
            None
        };

        let (provider_name, model_id, provider, explicit_provider) =
            self.resolve_provider(&request.model, &request, &context).await?;

        // Resolve API key based on billing mode
        self.resolve_api_key_for_request(&mut context, &provider_name)?;

        // Pre-request credit check for managed billing
        #[cfg(feature = "billing")]
        let estimated_cost = self.estimate_request_cost(&context, &provider_name, &request);

        #[cfg(feature = "billing")]
        if let Some(cost) = estimated_cost {
            self.check_credits(&context, cost).await?;
        }

        // Skip failover when the user explicitly selected a provider (e.g.
        // "nvidia/moonshotai/kimi-k2.5"), surface the error instead of
        // silently routing to a different model
        let response = if explicit_provider {
            self.complete_direct(&request, &context, &provider_name, &model_id, &provider)
                .await?
        } else {
            self.complete_with_failover(&request, &context, &provider_name, &model_id, &provider)
                .await?
        };

        // Record usage for billing
        #[cfg(feature = "billing")]
        if let Some(ref recorder) = self.inner.usage_recorder
            && let Some(ref usage) = response.usage
        {
            dispatch_usage_event(
                recorder,
                &context,
                &provider_name,
                &model_id,
                usage.prompt_tokens,
                usage.completion_tokens,
                &self.inner.model_registry,
                &self.inner.managed_margins,
                &self.inner.tier_margins,
            );
        }

        // Post-completion credit deduction based on actual usage
        #[cfg(feature = "billing")]
        if let Some(ref usage) = response.usage {
            self.deduct_credits_for_usage(
                &context,
                &provider_name,
                &model_id,
                usage.prompt_tokens,
                usage.completion_tokens,
            )
            .await;
        }

        // Report usage to synapse-api for dashboard charts
        if let Some(ref usage) = response.usage {
            dispatch_usage_report(
                &context,
                &provider_name,
                &model_id,
                usage.prompt_tokens,
                usage.completion_tokens,
                &self.inner.model_registry,
                &self.inner.managed_margins,
                &self.inner.tier_margins,
            );
        }

        // Store successful response in cache
        #[cfg(feature = "cache")]
        if let Some(ref key) = cache_key
            && let Some(ref cache) = self.inner.response_cache
            && let Ok(body) = serde_json::to_string(&response)
        {
            let entry = synapse_cache::CachedResponse {
                body,
                model: model_id.clone(),
                provider: provider_name.clone(),
            };
            if let Err(e) = cache.put(key, &entry, None).await {
                tracing::warn!(error = %e, "failed to cache response");
            }
        }

        Ok(response)
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
    ) -> Result<
        (
            String,
            Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>,
        ),
        LlmError,
    > {
        let original_model = request.model.clone();
        let (provider_name, model_id, provider, explicit_provider) =
            self.resolve_provider(&request.model, &request, &context).await?;

        // Resolve API key based on billing mode
        self.resolve_api_key_for_request(&mut context, &provider_name)?;

        // Pre-request credit check for managed billing
        #[cfg(feature = "billing")]
        let estimated_cost = self.estimate_request_cost(&context, &provider_name, &request);

        #[cfg(feature = "billing")]
        if let Some(cost) = estimated_cost {
            self.check_credits(&context, cost).await?;
        }

        // Skip failover/cascade when the user explicitly selected a
        // provider, surface the error instead of silently routing to a
        // different model
        let (actual_model, stream) = if explicit_provider {
            self.complete_stream_direct(&request, &context, &provider_name, &model_id, &provider)
                .await?
        } else if self.is_cascade_strategy(&original_model) {
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

        // Wrap stream to intercept usage events for billing and reporting
        let usage_reporter = context.parts.extensions.get::<synapse_auth::UsageReporter>().cloned();
        let resolved_key = context.parts.extensions.get::<synapse_auth::ResolvedKey>().cloned();

        #[cfg(feature = "billing")]
        if let Some(ref recorder) = self.inner.usage_recorder {
            let recorder = recorder.clone();
            let inner = Arc::clone(&self.inner);
            let ctx = context.clone();
            let prov = provider_name.clone();
            let mdl = model_id.clone();
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
                        &inner.managed_margins,
                        &inner.tier_margins,
                    );

                    // Deduct credits for actual usage (fire-and-forget)
                    if let Some(ref client) = billing_client {
                        spawn_credit_deduction(
                            client.clone(),
                            &ctx,
                            &prov,
                            &mdl,
                            usage.prompt_tokens,
                            usage.completion_tokens,
                            &inner.model_registry,
                            &inner.managed_margins,
                            &inner.tier_margins,
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
                            &inner.managed_margins,
                            &inner.tier_margins,
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
            let prov = provider_name.clone();
            let mdl = model_id.clone();

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
                        &inner.managed_margins,
                        &inner.tier_margins,
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
                managed_margins: HashMap::new(),
                tier_margins: HashMap::new(),
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
    pub fn set_managed_providers(&mut self, keys: HashMap<String, SecretString>, margins: HashMap<String, f64>) {
        let inner = Arc::get_mut(&mut self.inner).expect("set_managed_providers must be called before state is shared");
        inner.managed_keys = keys;
        inner.managed_margins = margins;
    }

    /// Configure per-tier margin multipliers for billing
    ///
    /// Tier margins take precedence over per-provider margins when the
    /// user's plan matches a configured tier.
    ///
    /// Must be called before the state is shared with handlers.
    ///
    /// # Panics
    ///
    /// Panics if called after the inner `Arc` has been cloned
    pub fn set_tier_margins(&mut self, tier_margins: HashMap<String, f64>) {
        let inner = Arc::get_mut(&mut self.inner).expect("set_tier_margins must be called before state is shared");
        inner.tier_margins = tier_margins;
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
                .check_entitlement(&identity.entity_type, &identity.entity_id, "smart_routing")
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
    ) -> Result<CompletionResponse, LlmError> {
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
                Ok(response)
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
    ) -> Result<
        (
            String,
            Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>,
        ),
        LlmError,
    > {
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
                Ok((model_id.to_owned(), stream))
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
    ) -> Result<CompletionResponse, LlmError> {
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
                Ok(response)
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

                let alternatives =
                    ModelRouter::find_equivalents(provider_name, model_id, &self.inner.failover.equivalence_groups);

                // max_attempts includes the primary, so remaining = max_attempts - 1
                let remaining = self.inner.failover.max_attempts.saturating_sub(1);
                let mut last_error = e;

                for (alt_provider, alt_model) in alternatives.into_iter().take(remaining) {
                    if !self.inner.health.is_available(&alt_provider) {
                        tracing::debug!(
                            provider = %alt_provider,
                            "skipping unhealthy provider"
                        );
                        continue;
                    }

                    let Some(alt_provider_impl) = self.inner.providers.get(&alt_provider) else {
                        continue;
                    };

                    tracing::warn!(
                        from_provider = provider_name,
                        to_provider = %alt_provider,
                        to_model = %alt_model,
                        "failing over to alternative provider"
                    );

                    let mut alt_req = request.clone();
                    alt_req.model.clone_from(&alt_model);

                    match alt_provider_impl.complete(&alt_req, context).await {
                        Ok(response) => {
                            self.inner.health.record_success(&alt_provider);
                            return Ok(response);
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
    ) -> Result<
        (
            String,
            Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>,
        ),
        LlmError,
    > {
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
                Ok((model_id.to_owned(), stream))
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

                let alternatives =
                    ModelRouter::find_equivalents(provider_name, model_id, &self.inner.failover.equivalence_groups);

                let remaining = self.inner.failover.max_attempts.saturating_sub(1);
                let mut last_error = e;

                for (alt_provider, alt_model) in alternatives.into_iter().take(remaining) {
                    if !self.inner.health.is_available(&alt_provider) {
                        continue;
                    }

                    let Some(alt_provider_impl) = self.inner.providers.get(&alt_provider) else {
                        continue;
                    };

                    tracing::warn!(
                        from_provider = provider_name,
                        to_provider = %alt_provider,
                        to_model = %alt_model,
                        "failing over streaming to alternative provider"
                    );

                    let mut alt_req = request.clone();
                    alt_req.model.clone_from(&alt_model);

                    match alt_provider_impl.complete_stream(&alt_req, context).await {
                        Ok(stream) => {
                            self.inner.health.record_success(&alt_provider);
                            return Ok((alt_model, stream));
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
    ) -> Result<
        (
            String,
            Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>,
        ),
        LlmError,
    > {
        // Get the escalation model from the routing decision's alternatives
        let escalation = self.resolve_escalation_model(cascade_config)?;

        // Stream from the initial (cheap) model
        let (initial_model, mut stream) = self
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
                        model = %initial_model,
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
                            model = %initial_model,
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
            let remaining = stream;
            let replay: Vec<Result<StreamEvent, LlmError>> = buffered_events.into_iter().map(Ok).collect();
            let replay_stream = futures_util::stream::iter(replay);
            let combined = replay_stream.chain(remaining);
            return Ok((initial_model, Box::pin(combined)));
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
                model = %initial_model,
                "cascade: initial model response is confident, replaying buffer"
            );
            let replay: Vec<Result<StreamEvent, LlmError>> = buffered_events.into_iter().map(Ok).collect();
            return Ok((initial_model, Box::pin(futures_util::stream::iter(replay))));
        }

        // Not confident, escalate to stronger model
        tracing::info!(
            initial_model = %initial_model,
            escalation_provider = %escalation.0,
            escalation_model = %escalation.1,
            "cascade: escalating to stronger model"
        );

        let (esc_provider_name, esc_model_id) = &escalation;
        let esc_provider = self
            .inner
            .providers
            .get(esc_provider_name)
            .ok_or_else(|| LlmError::ProviderNotFound {
                provider: esc_provider_name.clone(),
            })?;

        self.complete_stream_with_failover(request, context, esc_provider_name, esc_model_id, esc_provider)
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

    /// Resolve the API key for a request based on billing mode
    ///
    /// - BYOK: use the decrypted provider key from the resolved API key.
    ///   Fails if no key is registered for the target provider.
    /// - Managed: use the managed provider key from billing config
    /// - No billing identity: leave context unchanged (provider uses its
    ///   configured key)
    ///
    /// # Errors
    ///
    /// Returns `LlmError::InvalidRequest` when a BYOK user has no key
    /// registered for the target provider
    fn resolve_api_key_for_request(&self, context: &mut RequestContext, provider_name: &str) -> Result<(), LlmError> {
        use synapse_core::BillingMode;

        let Some(ref identity) = context.billing_identity else {
            return Ok(());
        };

        match identity.mode {
            BillingMode::Byok => {
                // Use decrypted BYOK key for this provider
                let key = context.provider_keys.get(provider_name).ok_or_else(|| {
                    LlmError::InvalidRequest(format!(
                        "no BYOK key registered for provider '{provider_name}'. \
                         Register one via setProviderKey or use a managed-mode API key"
                    ))
                })?;
                context.api_key = Some(key.clone());
            }
            BillingMode::Managed => {
                // Use the managed provider key
                if let Some(managed_key) = self.inner.managed_keys.get(provider_name) {
                    context.api_key = Some(managed_key.clone());
                }
            }
        }

        Ok(())
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

/// Estimate the cost of a request based on input tokens
///
/// Returns `None` if the user is not in managed billing mode
#[cfg(feature = "billing")]
impl LlmState {
    fn estimate_request_cost(
        &self,
        context: &RequestContext,
        provider_name: &str,
        request: &CompletionRequest,
    ) -> Option<f64> {
        let identity = context.billing_identity.as_ref()?;
        if identity.mode != synapse_core::BillingMode::Managed {
            return None;
        }

        // Estimate input tokens from message content
        let estimated_input: usize = request.messages.iter().map(|m| m.content.as_text().len() / 4).sum();

        // Estimate output tokens conservatively (use max_tokens if set, else default)
        let estimated_output = request.params.max_tokens.unwrap_or(1024) as usize;

        let plan = context
            .parts
            .extensions
            .get::<synapse_auth::ResolvedKey>()
            .map(|r| r.plan.as_str());
        let cost = self
            .inner
            .model_registry
            .find(provider_name, &request.model)
            .map_or(0.0, |profile| {
                let base = profile.estimate_cost(estimated_input, estimated_output);
                let margin = resolve_margin(
                    plan,
                    &self.inner.tier_margins,
                    &self.inner.managed_margins,
                    provider_name,
                );
                base * margin
            });

        if cost > 0.0 { Some(cost) } else { None }
    }

    /// Check if the user has sufficient credits for the estimated cost
    async fn check_credits(&self, context: &RequestContext, estimated_cost: f64) -> Result<(), LlmError> {
        let Some(ref client) = self.inner.billing_client else {
            return Ok(());
        };

        let Some(ref identity) = context.billing_identity else {
            return Ok(());
        };

        match client
            .check_credits(&identity.entity_type, &identity.entity_id, estimated_cost)
            .await
        {
            Ok(response) => {
                if !response.sufficient {
                    return Err(LlmError::InsufficientCredits {
                        message: format!(
                            "estimated cost ${:.4} exceeds available balance ${:.4}",
                            estimated_cost, response.balance
                        ),
                    });
                }
                Ok(())
            }
            Err(e) => match self.inner.billing_fail_mode {
                FailMode::Open => {
                    tracing::warn!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "credit check failed, allowing request (fail-open mode)"
                    );
                    Ok(())
                }
                FailMode::Closed => {
                    tracing::error!(
                        error = %e,
                        entity_id = %identity.entity_id,
                        "credit check failed, rejecting request (fail-closed mode)"
                    );
                    Err(LlmError::BillingUnavailable)
                }
            },
        }
    }

    /// Deduct credits after a successful completion based on actual usage
    async fn deduct_credits_for_usage(
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

        let plan = context
            .parts
            .extensions
            .get::<synapse_auth::ResolvedKey>()
            .map(|r| r.plan.as_str());
        let actual_cost = self
            .inner
            .model_registry
            .find(provider_name, model_id)
            .map_or(0.0, |profile| {
                let base = profile.estimate_cost(input_tokens as usize, output_tokens as usize);
                let margin = resolve_margin(
                    plan,
                    &self.inner.tier_margins,
                    &self.inner.managed_margins,
                    provider_name,
                );
                base * margin
            });

        if actual_cost <= 0.0 {
            return;
        }

        let request = synapse_billing::CreditDeductRequest {
            amount: actual_cost,
            description: Some(format!("{provider_name}/{model_id}")),
            idempotency_key: Some(uuid::Uuid::new_v4().to_string()),
            reference_type: Some("completion".to_owned()),
            reference_id: None,
        };

        if let Err(e) = client
            .deduct_credits(&identity.entity_type, &identity.entity_id, &request)
            .await
        {
            tracing::warn!(
                error = %e,
                entity_id = %identity.entity_id,
                cost = actual_cost,
                "failed to deduct credits after completion"
            );
        }
    }
}

/// Spawn a fire-and-forget credit deduction task for streaming usage events
#[cfg(feature = "billing")]
#[allow(clippy::too_many_arguments)]
fn spawn_credit_deduction(
    client: synapse_billing::AetherClient,
    context: &RequestContext,
    provider_name: &str,
    model_id: &str,
    input_tokens: u32,
    output_tokens: u32,
    model_registry: &ModelRegistry,
    managed_margins: &HashMap<String, f64>,
    tier_margins: &HashMap<String, f64>,
) {
    let Some(ref identity) = context.billing_identity else {
        return;
    };

    if identity.mode != synapse_core::BillingMode::Managed {
        return;
    }

    let plan = context
        .parts
        .extensions
        .get::<synapse_auth::ResolvedKey>()
        .map(|r| r.plan.as_str());
    let actual_cost = model_registry.find(provider_name, model_id).map_or(0.0, |profile| {
        let base = profile.estimate_cost(input_tokens as usize, output_tokens as usize);
        let margin = resolve_margin(plan, tier_margins, managed_margins, provider_name);
        base * margin
    });

    if actual_cost <= 0.0 {
        return;
    }

    let entity_type = identity.entity_type.clone();
    let entity_id = identity.entity_id.clone();
    let desc = format!("{provider_name}/{model_id}");

    tokio::spawn(async move {
        let request = synapse_billing::CreditDeductRequest {
            amount: actual_cost,
            description: Some(desc),
            idempotency_key: Some(uuid::Uuid::new_v4().to_string()),
            reference_type: Some("completion".to_owned()),
            reference_id: None,
        };

        if let Err(e) = client.deduct_credits(&entity_type, &entity_id, &request).await {
            tracing::warn!(
                error = %e,
                entity_id = %entity_id,
                cost = actual_cost,
                "failed to deduct credits after streaming completion"
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
    managed_margins: &HashMap<String, f64>,
    tier_margins: &HashMap<String, f64>,
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
        let plan = context
            .parts
            .extensions
            .get::<synapse_auth::ResolvedKey>()
            .map(|r| r.plan.as_str());
        model_registry.find(provider_name, model_id).map_or(0.0, |profile| {
            let base = profile.estimate_cost(input_tokens as usize, output_tokens as usize);
            let margin = resolve_margin(plan, tier_margins, managed_margins, provider_name);
            base * margin
        })
    };

    let idempotency_key = uuid::Uuid::new_v4().to_string();

    recorder.record(synapse_billing::UsageEvent {
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
    managed_margins: &HashMap<String, f64>,
    tier_margins: &HashMap<String, f64>,
) {
    let Some(reporter) = context.parts.extensions.get::<synapse_auth::UsageReporter>() else {
        return;
    };

    let Some(resolved) = context.parts.extensions.get::<synapse_auth::ResolvedKey>() else {
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
        managed_margins,
        tier_margins,
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
    managed_margins: &HashMap<String, f64>,
    tier_margins: &HashMap<String, f64>,
) {
    let estimated_cost_usd = model_registry.find(provider_name, model_id).map_or(0.0, |profile| {
        let base = profile.estimate_cost(input_tokens as usize, output_tokens as usize);
        let margin = resolve_margin(Some(&resolved.plan), tier_margins, managed_margins, provider_name);
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

/// Resolve the effective margin for a request
///
/// Tier margin takes precedence over provider margin when the user's plan
/// matches a configured tier.
fn resolve_margin(
    plan: Option<&str>,
    tier_margins: &HashMap<String, f64>,
    managed_margins: &HashMap<String, f64>,
    provider_name: &str,
) -> f64 {
    plan.and_then(|p| tier_margins.get(p).copied())
        .or_else(|| managed_margins.get(provider_name).copied())
        .unwrap_or(1.0)
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

    #[test]
    fn resolve_margin_tier_takes_precedence() {
        let mut tier_margins = HashMap::new();
        tier_margins.insert("free".to_owned(), 1.2);
        tier_margins.insert("pro".to_owned(), 1.15);

        let mut managed_margins = HashMap::new();
        managed_margins.insert("openai".to_owned(), 1.3);

        // Tier margin wins when plan matches
        let m = resolve_margin(Some("free"), &tier_margins, &managed_margins, "openai");
        assert!((m - 1.2).abs() < f64::EPSILON);

        // Falls back to provider margin when plan has no tier margin
        let m = resolve_margin(Some("enterprise"), &tier_margins, &managed_margins, "openai");
        assert!((m - 1.3).abs() < f64::EPSILON);

        // Falls back to provider margin when no plan
        let m = resolve_margin(None, &tier_margins, &managed_margins, "openai");
        assert!((m - 1.3).abs() < f64::EPSILON);

        // Falls back to 1.0 when nothing matches
        let m = resolve_margin(None, &tier_margins, &managed_margins, "unknown");
        assert!((m - 1.0).abs() < f64::EPSILON);
    }
}
