use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use synapse_auth::{ApiKeyResolver, KeyMode, UsageReporter, VaultClient};
use synapse_core::{BillingIdentity, BillingMode};

/// Vault-resolved provider keys stored as a request extension
///
/// When present, these override the synapse-api-provided `provider_keys`
/// in the `RequestContext`
#[derive(Clone, Debug)]
pub struct VaultProviderKeys(pub HashMap<String, SecretString>);

/// Authenticate requests via API key
///
/// Extracts Bearer token from Authorization header, which must use the
/// `synapse_` prefix. Rejects requests without a valid token unless the
/// path is in the public paths list.
///
/// When a `VaultClient` is provided and the key mode is BYOK, provider
/// keys are resolved from Gatekeeper's vault as an overlay
pub async fn auth_middleware(
    resolver: ApiKeyResolver,
    vault_client: Option<Arc<VaultClient>>,
    public_paths: Vec<String>,
    usage_reporter: Option<UsageReporter>,
    request: Request,
    next: Next,
) -> Response {
    // Allow CORS preflight through without auth so the CORS layer can respond
    if request.method() == http::Method::OPTIONS {
        return next.run(request).await;
    }

    let path = request.uri().path().to_string();

    if public_paths.iter().any(|p| path.starts_with(p)) {
        return next.run(request).await;
    }

    let token = request
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let Some(token) = token else {
        return (StatusCode::UNAUTHORIZED, "missing Authorization header").into_response();
    };

    if !token.starts_with("synapse_") {
        return (StatusCode::UNAUTHORIZED, "invalid API key format").into_response();
    }

    match resolver.resolve(token).await {
        Ok(result) => {
            let billing_identity = BillingIdentity {
                entity_type: "user".to_string(),
                entity_id: result.user_id.clone(),
                mode: match result.mode {
                    KeyMode::Byok => BillingMode::Byok,
                    KeyMode::Managed => BillingMode::Managed,
                },
            };

            let mut request = request;

            // Resolve BYOK keys from Gatekeeper vault when configured
            if result.mode == KeyMode::Byok
                && let Some(ref vault) = vault_client
            {
                let vault_keys = resolve_vault_keys(vault, &result.user_id, &result.provider_keys).await;
                if !vault_keys.is_empty() {
                    request.extensions_mut().insert(VaultProviderKeys(vault_keys));
                }
            }

            request.extensions_mut().insert(result);
            request.extensions_mut().insert(billing_identity);
            if let Some(reporter) = usage_reporter {
                request.extensions_mut().insert(reporter);
            }
            next.run(request).await
        }
        Err(e) => {
            tracing::warn!(error = %e, "API key authentication failed");
            (StatusCode::UNAUTHORIZED, "invalid API key").into_response()
        }
    }
}

/// Resolve BYOK provider keys from Gatekeeper's vault
///
/// Uses the provider list from synapse-api's resolved key to know which
/// providers to query. Vault keys overlay synapse-api keys: if the vault
/// has a key for a provider, it takes precedence
async fn resolve_vault_keys(
    vault: &VaultClient,
    user_id: &str,
    api_provider_keys: &[synapse_auth::ProviderKeyRef],
) -> HashMap<String, SecretString> {
    // Collect provider names that the user has configured
    let providers: Vec<&str> = api_provider_keys.iter().map(|pk| pk.provider.as_str()).collect();

    if providers.is_empty() {
        return HashMap::new();
    }

    let vault_keys = vault.resolve_all(user_id, &providers).await;

    let mut keys = HashMap::with_capacity(vault_keys.len());
    for vk in vault_keys {
        keys.insert(
            vk.provider.clone(),
            SecretString::from(vk.key.expose_secret().to_string()),
        );
    }

    if !keys.is_empty() {
        tracing::debug!(
            user_id,
            providers = ?keys.keys().collect::<Vec<_>>(),
            "resolved BYOK keys from vault"
        );
    }

    keys
}
