use std::sync::Arc;
use std::time::Duration;

use mini_moka::sync::Cache;
use sha2::{Digest, Sha256};
use synapse_config::{McpCacheConfig, McpServerType};

use crate::downstream::client::McpClient;
use crate::error::McpError;

/// LRU cache for dynamically-created MCP downstream connections
pub struct DownstreamCache {
    cache: Cache<String, Arc<McpClient>>,
}

impl DownstreamCache {
    /// Create a new cache with the given configuration
    pub fn new(config: &McpCacheConfig) -> Self {
        let cache = Cache::builder()
            .max_capacity(config.max_connections)
            .time_to_idle(Duration::from_secs(config.ttl))
            .build();

        Self { cache }
    }

    /// Get a cached client or connect a new one
    pub async fn get_or_connect(&self, name: &str, server_type: &McpServerType) -> Result<Arc<McpClient>, McpError> {
        let key = cache_key(name, server_type);

        if let Some(client) = self.cache.get(&key) {
            return Ok(client);
        }

        let client = Arc::new(McpClient::connect(name, server_type).await?);
        self.cache.insert(key, Arc::clone(&client));
        Ok(client)
    }

    /// Remove a client from the cache
    pub fn invalidate(&self, name: &str, server_type: &McpServerType) {
        let key = cache_key(name, server_type);
        self.cache.invalidate(&key);
    }

    /// Get the number of cached connections
    pub fn len(&self) -> u64 {
        self.cache.entry_count()
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Generate a cache key from server name and transport configuration
fn cache_key(name: &str, server_type: &McpServerType) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());

    match server_type {
        McpServerType::Stdio(cfg) => {
            hasher.update(b"stdio:");
            hasher.update(cfg.command.as_bytes());
            for arg in &cfg.args {
                hasher.update(arg.as_bytes());
            }
        }
        McpServerType::Sse(cfg) => {
            hasher.update(b"sse:");
            hasher.update(cfg.url.as_str().as_bytes());
        }
        McpServerType::StreamableHttp(cfg) => {
            hasher.update(b"streamable:");
            hasher.update(cfg.url.as_str().as_bytes());
        }
    }

    format!("{:x}", hasher.finalize())
}
