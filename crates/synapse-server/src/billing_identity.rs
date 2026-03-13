use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use synapse_config::{BillingConfig, OperatingMode};
use synapse_core::{Authentication, BillingIdentity, BillingMode};

/// Header that clients use to forward their own provider API key
const PROVIDER_KEY_HEADER: &str = "x-provider-api-key";

/// Middleware that resolves billing identity from the JWT
///
/// Extracts the `sub` claim from the validated JWT token and
/// determines the billing mode based on whether the user provided
/// their own provider key
pub async fn billing_identity_middleware(
    config: BillingConfig,
    request: Request,
    next: Next,
) -> Response {
    let auth = request.extensions().get::<Authentication>().cloned();

    let Some(auth) = auth else {
        return (StatusCode::UNAUTHORIZED, "billing enabled but no authentication present").into_response();
    };

    let Some(ref token) = auth.synapse else {
        return (StatusCode::UNAUTHORIZED, "billing enabled but no valid JWT present").into_response();
    };

    let Some(ref subject) = token.claims().custom.subject else {
        return (StatusCode::UNAUTHORIZED, "JWT missing sub claim for billing identity").into_response();
    };

    // Determine billing mode
    let has_provider_key = request.headers().contains_key(PROVIDER_KEY_HEADER);
    let mode = resolve_billing_mode(&config.mode, has_provider_key);

    let identity = BillingIdentity {
        entity_type: config.entity_type.clone(),
        entity_id: subject.clone(),
        mode,
    };

    let mut request = request;
    request.extensions_mut().insert(identity);

    next.run(request).await
}

/// Determine billing mode from config and whether a provider key is present
const fn resolve_billing_mode(configured_mode: &OperatingMode, has_provider_key: bool) -> BillingMode {
    match configured_mode {
        OperatingMode::Byok => BillingMode::Byok,
        OperatingMode::Managed => BillingMode::Managed,
        OperatingMode::Hybrid => {
            if has_provider_key {
                BillingMode::Byok
            } else {
                BillingMode::Managed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byok_mode_always_returns_byok() {
        assert_eq!(
            resolve_billing_mode(&OperatingMode::Byok, false),
            BillingMode::Byok
        );
        assert_eq!(
            resolve_billing_mode(&OperatingMode::Byok, true),
            BillingMode::Byok
        );
    }

    #[test]
    fn managed_mode_always_returns_managed() {
        assert_eq!(
            resolve_billing_mode(&OperatingMode::Managed, false),
            BillingMode::Managed
        );
        assert_eq!(
            resolve_billing_mode(&OperatingMode::Managed, true),
            BillingMode::Managed
        );
    }

    #[test]
    fn hybrid_mode_detects_from_provider_key() {
        assert_eq!(
            resolve_billing_mode(&OperatingMode::Hybrid, true),
            BillingMode::Byok
        );
        assert_eq!(
            resolve_billing_mode(&OperatingMode::Hybrid, false),
            BillingMode::Managed
        );
    }
}
