use std::borrow::Cow;
use std::sync::Arc;

use rmcp::model::{CallToolRequestParam, CallToolResult, Tool};
use rmcp::service::{RoleClient, RunningService, ServiceExt as _};
use rmcp::transport::TokioChildProcess;
use synapse_config::{HttpConfig, McpAuthConfig, McpServerType, StdioConfig};
use tokio::sync::Mutex;

use crate::error::McpError;

/// Connected MCP downstream client wrapping a running rmcp service
pub struct McpClient {
    service: Mutex<RunningService<RoleClient, ()>>,
    server_name: String,
    server_config: McpServerType,
}

impl McpClient {
    /// Connect to a downstream MCP server
    pub async fn connect(name: &str, server_type: &McpServerType) -> Result<Self, McpError> {
        let service = match server_type {
            McpServerType::Stdio(config) => Self::connect_stdio(config).await?,
            McpServerType::Sse(config) => Self::connect_sse(config).await?,
            McpServerType::StreamableHttp(config) => Self::connect_streamable_http(config).await?,
        };

        tracing::info!(server = name, "connected to MCP server");

        Ok(Self {
            service: Mutex::new(service),
            server_name: name.to_string(),
            server_config: server_type.clone(),
        })
    }

    async fn connect_stdio(config: &StdioConfig) -> Result<RunningService<RoleClient, ()>, McpError> {
        let mut cmd = tokio::process::Command::new(&config.command);
        cmd.args(&config.args);
        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        let transport =
            TokioChildProcess::new(cmd).map_err(|e| McpError::Transport(format!("failed to spawn process: {e}")))?;

        ().serve(transport)
            .await
            .map_err(|e| McpError::Transport(format!("STDIO handshake failed: {e}")))
    }

    async fn connect_sse(config: &HttpConfig) -> Result<RunningService<RoleClient, ()>, McpError> {
        use rmcp::transport::SseClientTransport;
        use rmcp::transport::sse_client::SseClientConfig;

        let sse_config = SseClientConfig {
            sse_endpoint: Arc::from(config.url.as_str()),
            ..Default::default()
        };

        // Build client with auth header if configured
        let client = build_reqwest_client(config.auth.as_ref())?;

        let transport = SseClientTransport::start_with_client(client, sse_config)
            .await
            .map_err(|e| McpError::Transport(format!("SSE connection failed: {e}")))?;

        ().serve(transport)
            .await
            .map_err(|e| McpError::Transport(format!("SSE handshake failed: {e}")))
    }

    async fn connect_streamable_http(config: &HttpConfig) -> Result<RunningService<RoleClient, ()>, McpError> {
        use rmcp::transport::StreamableHttpClientTransport;
        use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;

        let mut transport_config = StreamableHttpClientTransportConfig::with_uri(config.url.as_str());

        // Apply auth header if configured
        if let Some(McpAuthConfig::Token { ref token }) = config.auth {
            use secrecy::ExposeSecret;
            transport_config = transport_config.auth_header(format!("Bearer {}", token.expose_secret()));
        }

        let client = reqwest::Client::new();
        let transport = StreamableHttpClientTransport::with_client(client, transport_config);

        ().serve(transport)
            .await
            .map_err(|e| McpError::Transport(format!("StreamableHTTP handshake failed: {e}")))
    }

    /// List all tools available on this server
    pub async fn list_tools(&self) -> Result<Vec<Tool>, McpError> {
        self.service
            .lock()
            .await
            .list_all_tools()
            .await
            .map_err(|e| McpError::Transport(format!("list_tools failed on {}: {e}", self.server_name)))
    }

    /// Call a tool on this server, reconnecting once on transport failure
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<CallToolResult, McpError> {
        // First attempt — clone arguments so we retain them for the retry
        let first_result = {
            let guard = self.service.lock().await;
            guard
                .call_tool(CallToolRequestParam {
                    name: Cow::Owned(name.to_string()),
                    arguments: arguments.clone(),
                })
                .await
        };

        if let Ok(result) = first_result {
            return Ok(result);
        }

        // Transport failure — reconnect and retry once
        tracing::warn!(server = %self.server_name, "MCP transport failure, reconnecting");

        let new_service = match &self.server_config {
            McpServerType::Stdio(c) => Self::connect_stdio(c).await?,
            McpServerType::Sse(c) => Self::connect_sse(c).await?,
            McpServerType::StreamableHttp(c) => Self::connect_streamable_http(c).await?,
        };

        let mut guard = self.service.lock().await;
        *guard = new_service;

        guard
            .call_tool(CallToolRequestParam {
                name: Cow::Owned(name.to_string()),
                arguments,
            })
            .await
            .map_err(|e| {
                McpError::Execution(format!(
                    "tool '{}' failed on {} after reconnect: {e}",
                    name, self.server_name
                ))
            })
    }

    /// Get the server name
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Gracefully shut down the connection
    pub async fn shutdown(self) -> Result<(), McpError> {
        self.service
            .into_inner()
            .cancel()
            .await
            .map_err(|e| McpError::Transport(format!("shutdown failed: {e}")))?;
        Ok(())
    }
}

/// Build a reqwest client with optional auth headers
fn build_reqwest_client(auth: Option<&McpAuthConfig>) -> Result<reqwest::Client, McpError> {
    let mut builder = reqwest::Client::builder();

    if let Some(McpAuthConfig::Token { token }) = auth {
        use secrecy::ExposeSecret;
        let mut headers = reqwest::header::HeaderMap::new();
        let val = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token.expose_secret()))
            .map_err(|e| McpError::Transport(format!("invalid auth token: {e}")))?;
        headers.insert(reqwest::header::AUTHORIZATION, val);
        builder = builder.default_headers(headers);
    }

    builder
        .build()
        .map_err(|e| McpError::Transport(format!("failed to build HTTP client: {e}")))
}
