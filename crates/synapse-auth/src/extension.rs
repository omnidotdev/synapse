//! The resolved API key as a request extension
//!
//! Request extensions are keyed by exact type, so a producer storing
//! `Arc<ResolvedKey>` and a consumer reading `ResolvedKey` silently miss each
//! other. Both sides go through these accessors so they can never drift

use std::sync::Arc;

use http::Extensions;

use crate::ResolvedKey;

/// Store the key a request authenticated with
pub fn insert_resolved_key(extensions: &mut Extensions, key: Arc<ResolvedKey>) {
    extensions.insert(key);
}

/// The key a request authenticated with, if it was authenticated by API key
#[must_use]
pub fn resolved_key(extensions: &Extensions) -> Option<&Arc<ResolvedKey>> {
    extensions.get::<Arc<ResolvedKey>>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Arc<ResolvedKey> {
        Arc::new(
            serde_json::from_value(serde_json::json!({
                "userId": "usr_1",
                "workspaceId": null,
                "apiKeyId": "key_1",
                "mode": "managed",
                "plan": "pro"
            }))
            .expect("valid resolved key"),
        )
    }

    #[test]
    fn a_stored_key_reads_back() {
        let mut extensions = Extensions::new();

        insert_resolved_key(&mut extensions, key());

        assert_eq!(resolved_key(&extensions).map(|k| k.api_key_id.as_str()), Some("key_1"));
    }

    #[test]
    fn an_unauthenticated_request_has_no_key() {
        assert!(resolved_key(&Extensions::new()).is_none());
    }
}
