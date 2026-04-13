use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::McpState;
use crate::error::McpError;

/// Build the MCP router
pub fn mcp_router(state: Arc<McpState>) -> Router {
    Router::new()
        .route("/mcp/tools/list", post(list_tools))
        .route("/mcp/tools/call", post(call_tool))
        .route("/mcp/search", get(search_tools))
        .with_state(state)
}

/// Request to list tools with optional server filter
#[derive(Debug, Deserialize)]
struct ListToolsRequest {
    /// Filter to a specific server
    #[serde(default)]
    server: Option<String>,
}

/// Tool info returned to clients
#[derive(Debug, Serialize)]
struct ToolInfo {
    name: String,
    server: String,
    description: String,
    input_schema: serde_json::Value,
}

/// Response containing available tools
#[derive(Debug, Serialize)]
struct ListToolsResponse {
    tools: Vec<ToolInfo>,
}

async fn list_tools(
    State(state): State<Arc<McpState>>,
    Json(req): Json<ListToolsRequest>,
) -> Result<Json<ListToolsResponse>, McpErrorResponse> {
    let tools = state.downstream.tools();

    let filtered: Vec<ToolInfo> = tools
        .iter()
        .filter(|t| req.server.as_ref().is_none_or(|s| s == &t.server_name))
        .filter(|t| state.access.check(&t.server_name, &t.original_name).is_ok())
        .map(|t| ToolInfo {
            name: t.qualified_name.clone(),
            server: t.server_name.clone(),
            description: t.description.clone(),
            input_schema: t.input_schema.clone(),
        })
        .collect();

    Ok(Json(ListToolsResponse { tools: filtered }))
}

/// Request to call a tool
#[derive(Debug, Deserialize)]
struct CallToolRequest {
    /// Qualified tool name (`server__tool`)
    name: String,
    /// Tool arguments
    #[serde(default)]
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Response from a tool call
#[derive(Debug, Serialize)]
struct CallToolResponse {
    content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
}

/// Simplified content block for API responses
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text { text: String },
    Image { data: String, mime_type: String },
}

async fn call_tool(
    State(state): State<Arc<McpState>>,
    Json(req): Json<CallToolRequest>,
) -> Result<Json<CallToolResponse>, McpErrorResponse> {
    // Parse and validate the qualified tool name
    let (server_name, tool_name) = crate::downstream::manager::DownstreamManager::parse_tool_name(&req.name)
        .ok_or_else(|| McpError::ToolNotFound { tool: req.name.clone() })?;

    // Check access control
    state.access.check(server_name, tool_name)?;

    // Execute the tool call
    let result = state.downstream.call_tool(&req.name, req.arguments).await?;

    // Convert rmcp content to our API format
    let content = result
        .content
        .into_iter()
        .filter_map(|c| {
            let raw = c.raw;
            match raw {
                rmcp::model::RawContent::Text(t) => Some(ContentBlock::Text { text: t.text }),
                rmcp::model::RawContent::Image(img) => Some(ContentBlock::Image {
                    data: img.data,
                    mime_type: img.mime_type,
                }),
                _ => None,
            }
        })
        .collect();

    Ok(Json(CallToolResponse {
        content,
        is_error: result.is_error,
    }))
}

/// Query parameters for tool search
#[derive(Debug, Deserialize)]
struct SearchQuery {
    /// Search query
    q: String,
    /// Maximum results (defaults to 10)
    #[serde(default = "default_limit")]
    limit: usize,
}

const fn default_limit() -> usize {
    10
}

/// Search result response
#[derive(Debug, Serialize)]
struct SearchResponse {
    results: Vec<crate::index::ToolSearchResult>,
}

async fn search_tools(
    State(state): State<Arc<McpState>>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, McpErrorResponse> {
    let Some(ref index) = state.tool_index else {
        return Ok(Json(SearchResponse { results: Vec::new() }));
    };

    let results = index.search(&query.q, query.limit)?;
    Ok(Json(SearchResponse { results }))
}

/// Error response wrapper that implements `IntoResponse`
struct McpErrorResponse(McpError);

impl From<McpError> for McpErrorResponse {
    fn from(e: McpError) -> Self {
        Self(e)
    }
}

impl IntoResponse for McpErrorResponse {
    fn into_response(self) -> axum::response::Response {
        use synapse_core::HttpError;

        let status = self.0.status_code();
        let body = serde_json::json!({
            "error": {
                "type": self.0.error_type(),
                "message": self.0.client_message(),
            }
        });

        (status, Json(body)).into_response()
    }
}
