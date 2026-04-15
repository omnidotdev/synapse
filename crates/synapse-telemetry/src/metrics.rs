//! Metric name constants and recording helpers

use std::time::Instant;

use opentelemetry::metrics::Histogram;

/// Record a duration measurement on a histogram
pub fn record_duration(histogram: &Histogram<f64>, start: Instant, attributes: &[opentelemetry::KeyValue]) {
    let duration = start.elapsed().as_secs_f64();
    histogram.record(duration, attributes);
}

// HTTP metric names
pub const HTTP_REQUEST_DURATION: &str = "http.server.request.duration";
pub const HTTP_REQUEST_COUNT: &str = "http.server.request.count";

// LLM metric names
pub const LLM_REQUEST_DURATION: &str = "llm.request.duration";
pub const LLM_REQUEST_COUNT: &str = "llm.request.count";
pub const LLM_TOKEN_USAGE: &str = "llm.token.usage";
pub const LLM_STREAMING_DURATION: &str = "llm.streaming.duration";
pub const LLM_TIME_TO_FIRST_TOKEN: &str = "llm.time_to_first_token";

// MCP metric names
pub const MCP_TOOL_CALL_DURATION: &str = "mcp.tool_call.duration";
pub const MCP_TOOL_CALL_COUNT: &str = "mcp.tool_call.count";
pub const MCP_SEARCH_DURATION: &str = "mcp.search.duration";
