use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use synapse_billing::AetherClient;
use synapse_config::{BillingConfig, FailMode, modality_display_name};
use synapse_core::BillingIdentity;

use crate::entitlement_cache::{CachedEntitlement, CachedUsageCheck, EntitlementCache};

/// Shared state for the entitlement middleware
#[derive(Clone)]
pub struct EntitlementState {
    client: Arc<AetherClient>,
    config: BillingConfig,
    cache: EntitlementCache,
}

impl EntitlementState {
    /// Create new entitlement state
    pub fn new(client: AetherClient, config: BillingConfig) -> Self {
        let cache = EntitlementCache::new(config.entitlement_cache_ttl_secs);

        Self {
            client: Arc::new(client),
            config,
            cache,
        }
    }

    /// Get a clone of the entitlement cache for sharing with other components
    pub(crate) fn cache(&self) -> EntitlementCache {
        self.cache.clone()
    }
}

/// Middleware that enforces entitlements and usage limits before processing
///
/// 1. Checks `api_access` entitlement → 403 if denied
/// 2. Checks modality entitlement if the route requires one → 403 if denied
/// 3. Checks usage limit for `requests` meter → 429 if exceeded
/// 4. Checks usage limit for `input_tokens` meter → 429 if exceeded
/// 5. Checks usage limit for `output_tokens` meter → 429 if exceeded
pub async fn entitlement_middleware(state: EntitlementState, request: Request, next: Next) -> Response {
    let Some(identity) = request.extensions().get::<BillingIdentity>() else {
        // No billing identity — request is unauthenticated or on a public path;
        // auth middleware already passed it through, so skip entitlement checks
        return next.run(request).await;
    };

    let entity_type = &identity.entity_type;
    let entity_id = &identity.entity_id;
    let path = request.uri().path().to_owned();

    // 1. Check api_access entitlement
    match check_entitlement(&state, entity_type, entity_id, &state.config.api_access_feature_key).await {
        Ok(true) => {}
        Ok(false) => {
            return (StatusCode::FORBIDDEN, "API access not granted for this account").into_response();
        }
        Err(e) => {
            return handle_aether_error(&state.config.fail_mode, e, request, next).await;
        }
    }

    // 2. Check modality entitlement if the route requires one
    if let Some(feature_key) = state.config.modality_feature_key(&path) {
        match check_entitlement(&state, entity_type, entity_id, feature_key).await {
            Ok(true) => {}
            Ok(false) => {
                let name = modality_display_name(feature_key);
                let msg = format!("{name} is not available on your current plan");
                return (StatusCode::FORBIDDEN, msg).into_response();
            }
            Err(e) => {
                return handle_aether_error(&state.config.fail_mode, e, request, next).await;
            }
        }
    }

    // 3. Check requests usage limit
    match check_usage(&state, entity_type, entity_id, &state.config.meters.requests, 1.0).await {
        Ok(true) => {}
        Ok(false) => {
            return (StatusCode::TOO_MANY_REQUESTS, "monthly request limit exceeded").into_response();
        }
        Err(e) => {
            return handle_aether_error(&state.config.fail_mode, e, request, next).await;
        }
    }

    // 4. Check input tokens usage limit
    // Uses 0.0 because we can't predict token count pre-request; a user who
    // exceeds their limit mid-request gets blocked on the next request
    match check_usage(&state, entity_type, entity_id, &state.config.meters.input_tokens, 0.0).await {
        Ok(true) => {}
        Ok(false) => {
            return (StatusCode::TOO_MANY_REQUESTS, "monthly input token limit exceeded").into_response();
        }
        Err(e) => {
            return handle_aether_error(&state.config.fail_mode, e, request, next).await;
        }
    }

    // 5. Check output tokens usage limit
    match check_usage(&state, entity_type, entity_id, &state.config.meters.output_tokens, 0.0).await {
        Ok(true) => {}
        Ok(false) => {
            return (StatusCode::TOO_MANY_REQUESTS, "monthly output token limit exceeded").into_response();
        }
        Err(e) => {
            return handle_aether_error(&state.config.fail_mode, e, request, next).await;
        }
    }

    next.run(request).await
}

/// Check an entitlement by feature key, using cache when available
async fn check_entitlement(
    state: &EntitlementState,
    entity_type: &str,
    entity_id: &str,
    feature_key: &str,
) -> Result<bool, synapse_billing::BillingError> {
    // Check cache first
    if let Some(cached) = state.cache.get_entitlement(entity_type, entity_id, feature_key) {
        return Ok(cached.has_access);
    }

    // Cache miss — call Aether
    let response = state
        .client
        .check_entitlement(entity_type, entity_id, feature_key)
        .await?;

    // Cache the result
    state.cache.put_entitlement(
        entity_type,
        entity_id,
        feature_key,
        CachedEntitlement {
            has_access: response.has_access,
            version: response.entitlement_version,
        },
    );

    Ok(response.has_access)
}

/// Check a usage meter limit, using cache when available
async fn check_usage(
    state: &EntitlementState,
    entity_type: &str,
    entity_id: &str,
    meter_key: &str,
    additional_usage: f64,
) -> Result<bool, synapse_billing::BillingError> {
    // Check cache first
    if let Some(cached) = state.cache.get_usage(entity_type, entity_id, meter_key) {
        return Ok(cached.allowed);
    }

    // Cache miss — call Aether
    let response = state
        .client
        .check_usage(entity_type, entity_id, meter_key, additional_usage)
        .await?;

    // Cache the result
    state.cache.put_usage(
        entity_type,
        entity_id,
        meter_key,
        CachedUsageCheck {
            allowed: response.allowed,
        },
    );

    Ok(response.allowed)
}

/// Handle an Aether communication error based on fail mode
async fn handle_aether_error(
    fail_mode: &FailMode,
    error: synapse_billing::BillingError,
    request: Request,
    next: Next,
) -> Response {
    match fail_mode {
        FailMode::Open => {
            tracing::warn!(
                error = %error,
                "Aether unreachable, allowing request through (fail-open mode)"
            );
            next.run(request).await
        }
        FailMode::Closed => {
            tracing::error!(
                error = %error,
                "Aether unreachable, rejecting request (fail-closed mode)"
            );
            (StatusCode::SERVICE_UNAVAILABLE, "billing service unavailable").into_response()
        }
    }
}
