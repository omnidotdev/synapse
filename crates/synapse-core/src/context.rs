use std::collections::HashMap;

use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Runtime context for provider requests
///
/// Shared across LLM, STT, and TTS request flows
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// HTTP request parts (method, URI, headers, extensions)
    pub parts: http::request::Parts,
    /// Upstream key that overrides the provider's configured key
    ///
    /// Written only by the gateway's billing-mode key resolution (the caller's
    /// BYOK key or the managed key for the target provider), never from client
    /// input. `None` means the provider uses its configured key
    pub api_key: Option<SecretString>,
    /// Client identity for rate limiting and access control
    pub client_identity: Option<ClientIdentity>,
    /// Authentication state from JWT/OAuth validation
    pub authentication: Authentication,
    /// Billing identity resolved from JWT, if billing is enabled
    pub billing_identity: Option<BillingIdentity>,
    /// Decrypted BYOK provider keys keyed by provider name
    pub provider_keys: HashMap<String, SecretString>,
}

impl RequestContext {
    /// Create a minimal context for embedded (non-HTTP) use
    ///
    /// Contains empty headers, no API key, no client identity, and
    /// default authentication state
    ///
    /// # Panics
    ///
    /// Panics if the HTTP request builder fails (should never happen
    /// with hardcoded valid values)
    #[must_use]
    pub fn empty() -> Self {
        let (parts, ()) = http::Request::builder()
            .method(http::Method::GET)
            .uri("/")
            .body(())
            .expect("valid minimal request")
            .into_parts();

        Self {
            parts,
            api_key: None,
            client_identity: None,
            authentication: Authentication::default(),
            billing_identity: None,
            provider_keys: HashMap::new(),
        }
    }

    /// Access request headers
    pub const fn headers(&self) -> &http::HeaderMap {
        &self.parts.headers
    }
}

/// A BYOK request targets a provider the caller has no key registered for
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingProviderKey {
    /// Provider the request targets
    pub provider: String,
}

impl std::fmt::Display for MissingProviderKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no BYOK key registered for provider '{}'. Register one via setProviderKey or use a managed-mode API key",
            self.provider
        )
    }
}

impl std::error::Error for MissingProviderKey {}

/// The caller's own key a request must send upstream to `provider`
///
/// A BYOK request is not metered, so it must run on the caller's key: never
/// the gateway's configured key, which would make it free inference. Managed
/// requests and requests without a billing identity return `None` and keep
/// the provider's configured key
///
/// # Errors
///
/// Returns `MissingProviderKey` for a BYOK request when the caller has no
/// key registered for `provider`
pub fn byok_provider_key<'a, S: std::hash::BuildHasher>(
    billing_identity: Option<&BillingIdentity>,
    provider_keys: &'a HashMap<String, SecretString, S>,
    provider: &str,
) -> Result<Option<&'a SecretString>, MissingProviderKey> {
    match billing_identity.map(|identity| &identity.mode) {
        Some(BillingMode::Byok) => provider_keys.get(provider).map(Some).ok_or_else(|| MissingProviderKey {
            provider: provider.to_owned(),
        }),
        Some(BillingMode::Managed) | None => Ok(None),
    }
}

/// Billing identity resolved from authentication state
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillingIdentity {
    /// Product this request bills against in Aether (the `appId` path segment).
    ///
    /// Resolved per request: a provisioned product key carries its own
    /// `app_id` (e.g. "facet"); absent that, it falls back to the gateway's
    /// configured default app so existing single-product deployments are
    /// unchanged.
    pub app_id: String,
    /// Entity type (e.g. "user")
    pub entity_type: String,
    /// Entity identifier (e.g. user ID from JWT sub claim)
    pub entity_id: String,
    /// Billing mode for this request
    pub mode: BillingMode,
    /// Plan the key resolved to, used to pick the gateway's own tier margin
    /// (`None` when the plan is unknown, e.g. JWT-authenticated requests)
    pub plan: Option<String>,
}

/// Per-request token limits resolved from entitlements
///
/// Stored in request extensions by the entitlement middleware so that
/// downstream guardrails can enforce tier-specific token caps
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenLimits {
    /// Maximum input tokens per request (e.g. 8192 for free, 200000 for pro)
    pub max_input_tokens: usize,
    /// Maximum output tokens per request (e.g. 4096 for free, 32768 for pro)
    pub max_output_tokens: usize,
}

impl TokenLimits {
    /// Conservative free-tier defaults matching the Aether SSOT
    pub const FREE_TIER: Self = Self {
        max_input_tokens: 8192,
        max_output_tokens: 4096,
    };

    /// Emergency ceiling applied when Aether is unreachable under the Degraded
    /// fail mode. Tighter than free tier so requests still succeed (availability
    /// preserved) but each is bounded, capping total spend during an outage.
    pub const EMERGENCY: Self = Self {
        max_input_tokens: 2048,
        max_output_tokens: 1024,
    };
}

/// How this request should be billed
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BillingMode {
    /// User brings their own provider API key (no token metering)
    Byok,
    /// Synapse provides the API key and meters usage with margin
    Managed,
}

/// Identified client and their group membership
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIdentity {
    /// Client identifier (e.g. user ID, API key ID)
    pub client_id: String,
    /// Group the client belongs to (e.g. "free", "pro", "enterprise")
    pub group: Option<String>,
}

/// Authentication state extracted from incoming requests
#[derive(Default, Clone, Debug)]
pub struct Authentication {
    /// Validated Synapse JWT token, if present
    pub synapse: Option<SynapseToken>,
    /// Whether the request includes an Anthropic authorization header
    pub has_anthropic_authorization: bool,
}

/// Validated JWT token with raw and parsed representations
#[derive(Clone, Debug)]
pub struct SynapseToken {
    /// Raw token string (kept secret for forwarding)
    pub raw: SecretString,
    /// Parsed and validated JWT
    pub token: jwt_compact::Token<Claims>,
}

impl std::ops::Deref for SynapseToken {
    type Target = jwt_compact::Token<Claims>;
    fn deref(&self) -> &Self::Target {
        &self.token
    }
}

/// JWT claims supporting OAuth 2.0 scopes and custom fields
#[serde_with::serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Issuer
    #[serde(default, rename = "iss")]
    pub issuer: Option<String>,
    /// Audience (single value or array)
    #[serde_as(deserialize_as = "Option<serde_with::OneOrMany<_>>")]
    #[serde(default, rename = "aud")]
    pub audience: Option<Vec<String>>,
    /// Subject
    #[serde(default, rename = "sub")]
    pub subject: Option<String>,
    /// Additional claims for flexible access to custom fields
    #[serde(flatten)]
    pub additional: HashMap<String, Value>,
}

impl Claims {
    /// Extract a claim value by path, supporting nested claims
    ///
    /// Paths can be simple (e.g. "sub") or nested (e.g. "user.plan").
    #[must_use]
    pub fn get_claim(&self, path: &str) -> Option<String> {
        match path {
            "iss" => return self.issuer.clone(),
            "sub" => return self.subject.clone(),
            "aud" => {
                return self.audience.as_ref().and_then(|audiences| audiences.first().cloned());
            }
            _ => {}
        }

        let mut parts = path.split('.');
        let first = parts.next()?;
        let current = parts.fold(self.additional.get(first).unwrap_or(&Value::Null), |current, part| {
            current.get(part).unwrap_or(&Value::Null)
        });

        match current {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_context_has_no_auth() {
        let ctx = RequestContext::empty();
        assert!(ctx.api_key.is_none());
        assert!(ctx.client_identity.is_none());
        assert!(ctx.headers().is_empty());
    }

    fn identity(mode: BillingMode) -> BillingIdentity {
        BillingIdentity {
            app_id: "synapse".to_owned(),
            entity_type: "user".to_owned(),
            entity_id: "usr_1".to_owned(),
            mode,
            plan: None,
        }
    }

    fn keys() -> HashMap<String, SecretString> {
        HashMap::from([("openai".to_owned(), SecretString::from("byok-openai"))])
    }

    #[test]
    fn byok_request_uses_the_callers_key_for_the_provider() {
        use secrecy::ExposeSecret;

        let keys = keys();
        let key = byok_provider_key(Some(&identity(BillingMode::Byok)), &keys, "openai").unwrap();

        assert_eq!(key.map(ExposeSecret::expose_secret), Some("byok-openai"));
    }

    #[test]
    fn byok_request_without_a_key_for_the_provider_is_an_error() {
        let err = byok_provider_key(Some(&identity(BillingMode::Byok)), &keys(), "anthropic").unwrap_err();

        assert_eq!(err.provider, "anthropic");
        assert!(
            err.to_string()
                .contains("no BYOK key registered for provider 'anthropic'")
        );
    }

    #[test]
    fn managed_and_anonymous_requests_keep_the_configured_key() {
        let keys = keys();

        assert!(
            byok_provider_key(Some(&identity(BillingMode::Managed)), &keys, "openai")
                .unwrap()
                .is_none()
        );
        assert!(byok_provider_key(None, &keys, "openai").unwrap().is_none());
    }
}
