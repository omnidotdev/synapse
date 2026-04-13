use serde::Deserialize;
use tracing::info;

use crate::vault::VaultClient;

/// A vault change event emitted by Gatekeeper via Vortex
#[derive(Debug, Deserialize)]
pub struct VaultEvent {
    /// Event type (e.g. `gatekeeper.vault.key_upserted`)
    #[serde(rename = "type")]
    pub event_type: String,
    /// Payload describing which key changed
    pub data: VaultEventData,
}

/// Payload for a vault change event
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultEventData {
    /// User whose key changed
    pub user_id: String,
    /// Provider name (e.g. "anthropic", "openai")
    pub provider: String,
}

/// Handle a vault event by evicting the corresponding cache entry
///
/// Processes `gatekeeper.vault.key_upserted` and
/// `gatekeeper.vault.key_deleted` events. All other event types are
/// silently ignored.
pub fn handle_vault_event(vault: &VaultClient, event: &VaultEvent) {
    match event.event_type.as_str() {
        "gatekeeper.vault.key_upserted" | "gatekeeper.vault.key_deleted" => {
            vault.evict(&event.data.user_id, &event.data.provider);
            info!(
                user_id = %event.data.user_id,
                provider = %event.data.provider,
                event_type = %event.event_type,
                "evicted vault cache entry via event"
            );
        }
        _ => {
            // Ignore unrelated events
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_upsert_event() {
        let json = r#"{
            "type": "gatekeeper.vault.key_upserted",
            "data": {
                "userId": "user_123",
                "provider": "anthropic"
            }
        }"#;

        let event: VaultEvent = serde_json::from_str(json).unwrap();

        assert_eq!(event.event_type, "gatekeeper.vault.key_upserted");
        assert_eq!(event.data.user_id, "user_123");
        assert_eq!(event.data.provider, "anthropic");
    }

    #[test]
    fn deserialize_delete_event() {
        let json = r#"{
            "type": "gatekeeper.vault.key_deleted",
            "data": {
                "userId": "user_456",
                "provider": "openai"
            }
        }"#;

        let event: VaultEvent = serde_json::from_str(json).unwrap();

        assert_eq!(event.event_type, "gatekeeper.vault.key_deleted");
        assert_eq!(event.data.user_id, "user_456");
        assert_eq!(event.data.provider, "openai");
    }
}
