use std::collections::HashMap;

use rmcp::model::CallToolResult;
use synapse_config::McpConfig;

use super::client::McpClient;
use crate::error::McpError;

/// Tool descriptor with server origin
#[derive(Debug, Clone)]
pub struct AggregatedTool {
    /// Fully qualified name: `server_name__tool_name`
    pub qualified_name: String,
    /// Original tool name on the downstream server
    pub original_name: String,
    /// Server this tool belongs to
    pub server_name: String,
    /// Tool description
    pub description: String,
    /// JSON schema for tool input
    pub input_schema: serde_json::Value,
}

/// Separator between server name and tool name
const TOOL_SEPARATOR: &str = "__";

/// Manages connections to all configured MCP downstream servers
pub struct DownstreamManager {
    clients: HashMap<String, McpClient>,
    /// Cached aggregated tool list
    tools: Vec<AggregatedTool>,
}

impl DownstreamManager {
    /// Connect to all configured MCP servers
    ///
    /// Servers that fail to connect are logged and skipped rather than
    /// causing startup failure.
    pub async fn connect(config: &McpConfig) -> Self {
        let mut clients = HashMap::new();

        for (name, server_config) in &config.servers {
            match McpClient::connect(name, &server_config.server_type).await {
                Ok(client) => {
                    clients.insert(name.clone(), client);
                }
                Err(e) => {
                    tracing::warn!(
                        server = name,
                        error = %e,
                        "failed to connect to MCP server, skipping"
                    );
                }
            }
        }

        let mut manager = Self {
            clients,
            tools: Vec::new(),
        };
        manager.refresh_tools().await;
        manager
    }

    /// Refresh the aggregated tool list from all connected servers
    pub async fn refresh_tools(&mut self) {
        let mut tools = Vec::new();

        for (server_name, client) in &self.clients {
            match client.list_tools().await {
                Ok(server_tools) => {
                    for tool in server_tools {
                        let original_name = tool.name.to_string();
                        let qualified_name = format!("{server_name}{TOOL_SEPARATOR}{original_name}");
                        let description = tool.description.as_deref().unwrap_or("").to_string();
                        let input_schema = serde_json::to_value(&*tool.input_schema).unwrap_or_default();

                        tools.push(AggregatedTool {
                            qualified_name,
                            original_name,
                            server_name: server_name.clone(),
                            description,
                            input_schema,
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        server = server_name,
                        error = %e,
                        "failed to list tools from MCP server"
                    );
                }
            }
        }

        tracing::info!(count = tools.len(), "aggregated MCP tools from all servers");
        self.tools = tools;
    }

    /// Get all aggregated tools
    pub fn tools(&self) -> &[AggregatedTool] {
        &self.tools
    }

    /// Parse a qualified tool name into (`server_name`, `tool_name`)
    pub fn parse_tool_name(qualified: &str) -> Option<(&str, &str)> {
        qualified.split_once(TOOL_SEPARATOR)
    }

    /// Call a tool by its qualified name
    pub async fn call_tool(
        &self,
        qualified_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<CallToolResult, McpError> {
        let (server_name, tool_name) = Self::parse_tool_name(qualified_name).ok_or_else(|| McpError::ToolNotFound {
            tool: qualified_name.to_string(),
        })?;

        let client = self.clients.get(server_name).ok_or_else(|| McpError::ServerNotFound {
            server: server_name.to_string(),
        })?;

        client.call_tool(tool_name, arguments).await
    }

    /// Get a reference to a specific server client
    pub fn get_client(&self, server_name: &str) -> Option<&McpClient> {
        self.clients.get(server_name)
    }

    /// Get the number of connected servers
    pub fn server_count(&self) -> usize {
        self.clients.len()
    }
}
