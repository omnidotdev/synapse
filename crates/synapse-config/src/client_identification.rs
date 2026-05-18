use serde::Deserialize;

/// Configuration for identifying clients
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientIdentificationConfig {
    /// How to extract the client ID
    pub client_id: ClientIdSource,
    /// How to extract the client group
    #[serde(default)]
    pub group_id: Option<GroupIdSource>,
}

/// Source for extracting the client ID
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ClientIdSource {
    /// Extract from JWT claims
    JwtClaim {
        /// Claim path (e.g. "sub" or "user.id")
        path: String,
    },
    /// Extract from an HTTP header
    Header {
        /// Header name to read
        name: String,
    },
}

/// Source for extracting the client group
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum GroupIdSource {
    /// Extract from JWT claims
    JwtClaim {
        /// Claim path (e.g. "user.plan")
        path: String,
        /// Allowed group values
        #[serde(default)]
        allowed: Vec<String>,
    },
    /// Extract from an HTTP header
    Header {
        /// Header name to read
        name: String,
        /// Allowed group values
        #[serde(default)]
        allowed: Vec<String>,
    },
}
