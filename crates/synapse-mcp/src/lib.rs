#![allow(clippy::must_use_candidate, clippy::missing_errors_doc)]

pub mod access;
pub mod cache;
pub mod downstream;
pub mod error;
pub mod index;
pub mod router;

pub use error::McpError;
pub use router::mcp_router;

use access::AccessController;
use downstream::manager::DownstreamManager;
use index::ToolIndex;
use synapse_config::McpConfig;

/// Shared MCP subsystem state
pub struct McpState {
    /// Manages connections to downstream MCP servers
    pub downstream: DownstreamManager,
    /// Tool-level access controller
    pub access: AccessController,
    /// Full-text search index for tools (None if no tools available)
    pub tool_index: Option<ToolIndex>,
}

impl McpState {
    /// Initialize the MCP subsystem from configuration
    ///
    /// Connects to all configured servers, aggregates tools,
    /// and builds the search index.
    pub async fn new(config: &McpConfig) -> Result<Self, McpError> {
        let access = AccessController::new(&config.servers);
        let downstream = DownstreamManager::connect(config).await;

        // Build search index from aggregated tools
        let tool_index = if downstream.tools().is_empty() {
            None
        } else {
            Some(ToolIndex::build(downstream.tools())?)
        };

        tracing::info!(
            servers = downstream.server_count(),
            tools = downstream.tools().len(),
            "MCP subsystem initialized"
        );

        Ok(Self {
            downstream,
            access,
            tool_index,
        })
    }
}
