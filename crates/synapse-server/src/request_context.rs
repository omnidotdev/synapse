use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use secrecy::SecretString;
use synapse_auth::ResolvedKey;
use synapse_core::{Authentication, BillingIdentity, RequestContext};

use crate::auth::VaultProviderKeys;

/// Middleware that constructs a `RequestContext` from the incoming request
///
/// Extracts HTTP parts and any pre-populated authentication/identity
/// extensions into a unified context for downstream handlers. When vault-
/// resolved keys are present, they overlay the synapse-api-provided keys.
pub async fn request_context_middleware(request: Request, next: Next) -> Response {
    let (parts, body) = request.into_parts();

    let api_key = None;
    let client_identity = parts.extensions.get().cloned();
    let authentication = parts.extensions.get::<Authentication>().cloned().unwrap_or_default();
    let billing_identity = parts.extensions.get::<BillingIdentity>().cloned();

    // Extract BYOK provider keys from resolved API key context
    let mut provider_keys = parts
        .extensions
        .get::<Arc<ResolvedKey>>()
        .map(|resolved| {
            resolved
                .provider_keys
                .iter()
                .map(|pk| (pk.provider.clone(), SecretString::from(pk.decrypted_key.clone())))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    // Overlay vault-resolved keys when present (vault takes precedence)
    if let Some(vault_keys) = parts.extensions.get::<VaultProviderKeys>() {
        for (provider, key) in &vault_keys.0 {
            provider_keys.insert(provider.clone(), key.clone());
        }
    }

    let context = RequestContext {
        parts: parts.clone(),
        api_key,
        client_identity,
        authentication,
        billing_identity,
        provider_keys,
    };

    let mut request = Request::from_parts(parts, body);
    request.extensions_mut().insert(context);

    next.run(request).await
}
