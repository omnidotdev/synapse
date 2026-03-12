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
    /// User-provided API key that overrides the configured key
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

/// Billing identity resolved from authentication state
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillingIdentity {
    /// Entity type (e.g. "user")
    pub entity_type: String,
    /// Entity identifier (e.g. user ID from JWT sub claim)
    pub entity_id: String,
    /// Billing mode for this request
    pub mode: BillingMode,
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
}
