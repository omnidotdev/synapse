use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use secrecy::{ExposeSecret, SecretString};
use synapse_auth::{ApiKeyResolver, KeyMode, ResolvedKey, UsageReporter, VaultClient};
use synapse_core::{BillingIdentity, BillingMode};

/// Vault-resolved provider keys stored as a request extension
///
/// When present, these override the synapse-api-provided `provider_keys`
/// in the `RequestContext`
#[derive(Clone, Debug)]
pub struct VaultProviderKeys(pub HashMap<String, SecretString>);

/// Paths that also accept the key in `x-api-key`: the Anthropic-compatible
/// endpoint, where the default Anthropic SDK sends its key in that header
const X_API_KEY_PATHS: &[&str] = &["/v1/messages"];

/// Header Anthropic clients send their key in
const X_API_KEY: &str = "x-api-key";

/// The API key a request authenticates with, and whether it came from
/// `x-api-key`
///
/// A Bearer token always wins; `x-api-key` is read only on
/// [`X_API_KEY_PATHS`] and only when no Bearer token is present
fn request_api_key(headers: &http::HeaderMap, path: &str) -> Option<(String, bool)> {
    let bearer = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if let Some(token) = bearer {
        return Some((token.to_owned(), false));
    }

    if !X_API_KEY_PATHS.contains(&path) {
        return None;
    }

    headers
        .get(X_API_KEY)
        .and_then(|v| v.to_str().ok())
        .map(|v| (v.trim().to_owned(), true))
}

/// Authenticate requests via API key
///
/// Extracts the Bearer token from the Authorization header (or, on the
/// Anthropic-compatible `/v1/messages`, the `x-api-key` header), which must
/// use the `synapse_` prefix. Both are resolved and billed the same way.
/// Rejects requests without a valid key unless the path is in the public
/// paths list.
///
/// When a `VaultClient` is provided and the key mode is BYOK, provider
/// keys are resolved from Gatekeeper's vault as an overlay
pub async fn auth_middleware(
    resolver: ApiKeyResolver,
    vault_client: Option<Arc<VaultClient>>,
    public_paths: Vec<String>,
    usage_reporter: Option<UsageReporter>,
    default_app_id: String,
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

    let Some((token, from_x_api_key)) = request_api_key(request.headers(), &path) else {
        return (StatusCode::UNAUTHORIZED, "missing Authorization header").into_response();
    };

    if !token.starts_with("synapse_") {
        return (StatusCode::UNAUTHORIZED, "invalid API key format").into_response();
    }

    match resolver.resolve(&token).await {
        Ok(result) => {
            let billing_identity = resolve_billing_identity(&result, &default_app_id);

            let mut request = request;

            // The Synapse key never travels further than auth, so a header
            // forwarding rule can never hand it to a provider
            if from_x_api_key {
                request.headers_mut().remove(X_API_KEY);
            }

            // Resolve BYOK keys from Gatekeeper vault when configured
            if result.mode == KeyMode::Byok
                && let Some(ref vault) = vault_client
            {
                let vault_keys = resolve_vault_keys(vault, &result.user_id, &result.provider_keys).await;
                if !vault_keys.is_empty() {
                    request.extensions_mut().insert(VaultProviderKeys(vault_keys));
                }
            }

            // Every modality reports what it served through this sink, bound
            // to the key the request authenticated with
            if let Some(reporter) = usage_reporter {
                request.extensions_mut().insert(reporter.sink_for(Arc::clone(&result)));
            }
            synapse_auth::insert_resolved_key(request.extensions_mut(), result);
            request.extensions_mut().insert(billing_identity);
            next.run(request).await
        }
        Err(e) => {
            tracing::warn!(error = %e, "API key authentication failed");
            (StatusCode::UNAUTHORIZED, "invalid API key").into_response()
        }
    }
}

/// Resolve which product and entity a request bills against
///
/// A product-scoped key carries its own `app_id` and billing entity so
/// usage, credits, and entitlements resolve against that product. Absent
/// (direct Synapse customers, legacy keys) it falls back to the gateway's
/// configured default app and the key's own user as a `user` entity,
/// preserving single-product behavior
fn resolve_billing_identity(result: &ResolvedKey, default_app_id: &str) -> BillingIdentity {
    let app_id = result.app_id.clone().unwrap_or_else(|| default_app_id.to_owned());
    let (entity_type, entity_id) = result.billing_entity.as_ref().map_or_else(
        || ("user".to_owned(), result.user_id.clone()),
        |entity| (entity.entity_type.clone(), entity.entity_id.clone()),
    );

    BillingIdentity {
        app_id,
        entity_type,
        entity_id,
        mode: match result.mode {
            KeyMode::Byok => BillingMode::Byok,
            KeyMode::Managed | KeyMode::Manual => BillingMode::Managed,
        },
        plan: Some(result.plan.clone()),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a resolved key from the synapse-api wire shape so the tests also
    /// pin the camelCase contract (`appId`, `billingEntity`)
    fn key(json: serde_json::Value) -> ResolvedKey {
        serde_json::from_value(json).expect("valid resolved key")
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> http::HeaderMap {
        let mut map = http::HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, http::HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn bearer_is_read_on_every_path() {
        let h = headers(&[("authorization", "Bearer synapse_a")]);
        assert_eq!(
            request_api_key(&h, "/v1/chat/completions"),
            Some(("synapse_a".into(), false))
        );
        assert_eq!(request_api_key(&h, "/v1/messages"), Some(("synapse_a".into(), false)));
    }

    #[test]
    fn x_api_key_is_read_only_on_the_messages_endpoint() {
        let h = headers(&[("x-api-key", "synapse_a")]);
        assert_eq!(request_api_key(&h, "/v1/messages"), Some(("synapse_a".into(), true)));
        assert_eq!(request_api_key(&h, "/v1/chat/completions"), None);
        assert_eq!(request_api_key(&h, "/v1/models"), None);
        assert_eq!(request_api_key(&h, "/v1/messages/batches"), None);
    }

    #[test]
    fn bearer_wins_over_x_api_key() {
        let h = headers(&[("authorization", "Bearer synapse_a"), ("x-api-key", "synapse_b")]);
        assert_eq!(request_api_key(&h, "/v1/messages"), Some(("synapse_a".into(), false)));
    }

    #[test]
    fn product_scoped_key_bills_its_product_and_entity() {
        let resolved = key(serde_json::json!({
            "userId": "usr_owner",
            "workspaceId": null,
            "apiKeyId": "key_1",
            "mode": "managed",
            "plan": "free",
            "appId": "facet",
            "billingEntity": { "entityType": "user", "entityId": "usr_end" }
        }));

        let identity = resolve_billing_identity(&resolved, "synapse");

        assert_eq!(identity.app_id, "facet");
        assert_eq!(identity.entity_type, "user");
        assert_eq!(identity.entity_id, "usr_end");
        assert_eq!(identity.mode, BillingMode::Managed);
    }

    #[test]
    fn legacy_key_falls_back_to_default_app_and_owner() {
        let resolved = key(serde_json::json!({
            "userId": "usr_owner",
            "workspaceId": null,
            "apiKeyId": "key_2",
            "mode": "byok",
            "plan": "pro"
        }));

        let identity = resolve_billing_identity(&resolved, "synapse");

        assert_eq!(identity.app_id, "synapse");
        assert_eq!(identity.entity_type, "user");
        assert_eq!(identity.entity_id, "usr_owner");
        assert_eq!(identity.mode, BillingMode::Byok);
    }

    #[test]
    fn app_id_without_entity_bills_owner_under_product() {
        let resolved = key(serde_json::json!({
            "userId": "usr_owner",
            "workspaceId": null,
            "apiKeyId": "key_3",
            "mode": "manual",
            "plan": "team",
            "appId": "keystone"
        }));

        let identity = resolve_billing_identity(&resolved, "synapse");

        assert_eq!(identity.app_id, "keystone");
        assert_eq!(identity.entity_id, "usr_owner");
        assert_eq!(identity.mode, BillingMode::Managed);
    }
}
