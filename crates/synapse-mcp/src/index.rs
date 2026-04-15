use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{STORED, Schema, TEXT, Value as _};
use tantivy::{Index, IndexReader, ReloadPolicy, doc};

use crate::downstream::manager::AggregatedTool;
use crate::error::McpError;

/// Search result from the tool index
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolSearchResult {
    /// Fully qualified tool name
    pub qualified_name: String,
    /// Server name
    pub server_name: String,
    /// Original tool name
    pub tool_name: String,
    /// Tool description
    pub description: String,
    /// Relevance score
    pub score: f32,
}

/// Full-text search index for MCP tools backed by Tantivy
pub struct ToolIndex {
    reader: IndexReader,
    query_parser: QueryParser,
    schema: Schema,
}

impl ToolIndex {
    /// Build a search index from aggregated tools
    pub fn build(tools: &[AggregatedTool]) -> Result<Self, McpError> {
        let mut schema_builder = Schema::builder();
        let qualified_name_field = schema_builder.add_text_field("qualified_name", STORED);
        let tool_name_field = schema_builder.add_text_field("tool_name", TEXT | STORED);
        let description_field = schema_builder.add_text_field("description", TEXT | STORED);
        let server_name_field = schema_builder.add_text_field("server_name", STORED);
        let schema = schema_builder.build();

        let index = Index::create_in_ram(schema.clone());
        let mut writer = index
            .writer(15_000_000)
            .map_err(|e| McpError::Internal(anyhow::anyhow!("failed to create index writer: {e}")))?;

        for tool in tools {
            writer
                .add_document(doc!(
                    qualified_name_field => tool.qualified_name.as_str(),
                    tool_name_field => tool.original_name.as_str(),
                    description_field => tool.description.as_str(),
                    server_name_field => tool.server_name.as_str(),
                ))
                .map_err(|e| McpError::Internal(anyhow::anyhow!("failed to index tool: {e}")))?;
        }

        writer
            .commit()
            .map_err(|e| McpError::Internal(anyhow::anyhow!("failed to commit index: {e}")))?;

        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|e| McpError::Internal(anyhow::anyhow!("failed to create reader: {e}")))?;

        let query_parser = QueryParser::for_index(&index, vec![tool_name_field, description_field]);

        Ok(Self {
            reader,
            query_parser,
            schema,
        })
    }

    /// Search tools by query string
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<ToolSearchResult>, McpError> {
        let searcher = self.reader.searcher();
        let query = self
            .query_parser
            .parse_query(query)
            .map_err(|e| McpError::Internal(anyhow::anyhow!("invalid search query: {e}")))?;

        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(limit).order_by_score())
            .map_err(|e| McpError::Internal(anyhow::anyhow!("search failed: {e}")))?;

        let field_err = |name| McpError::Internal(anyhow::anyhow!("missing schema field: {name}"));
        let qualified_name_field = self
            .schema
            .get_field("qualified_name")
            .map_err(|_| field_err("qualified_name"))?;
        let tool_name_field = self.schema.get_field("tool_name").map_err(|_| field_err("tool_name"))?;
        let description_field = self
            .schema
            .get_field("description")
            .map_err(|_| field_err("description"))?;
        let server_name_field = self
            .schema
            .get_field("server_name")
            .map_err(|_| field_err("server_name"))?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (score, doc_address) in top_docs {
            let doc: tantivy::TantivyDocument = searcher
                .doc(doc_address)
                .map_err(|e| McpError::Internal(anyhow::anyhow!("failed to retrieve doc: {e}")))?;

            let get_text =
                |field| -> String { doc.get_first(field).and_then(|v| v.as_str()).unwrap_or("").to_string() };

            results.push(ToolSearchResult {
                qualified_name: get_text(qualified_name_field),
                server_name: get_text(server_name_field),
                tool_name: get_text(tool_name_field),
                description: get_text(description_field),
                score,
            });
        }

        Ok(results)
    }
}
