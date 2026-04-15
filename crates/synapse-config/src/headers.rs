use serde::Deserialize;

/// Header rule configuration for providers
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HeaderRuleConfig {
    /// Forward a header from the incoming request
    Forward(HeaderForwardConfig),
    /// Insert a static header
    Insert(HeaderInsertConfig),
    /// Remove a header
    Remove(HeaderRemoveConfig),
    /// Duplicate a header under a new name
    RenameDuplicate(HeaderRenameDuplicateConfig),
}

/// Forward a header, optionally renaming it
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderForwardConfig {
    /// Header name or glob pattern
    pub name: String,
    /// Rename the header
    #[serde(default)]
    pub rename: Option<String>,
    /// Default value if not present
    #[serde(default)]
    pub default: Option<String>,
}

/// Insert a static header
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderInsertConfig {
    /// Header name
    pub name: String,
    /// Header value
    pub value: String,
}

/// Remove a header by name or pattern
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderRemoveConfig {
    /// Header name or glob pattern
    pub name: String,
}

/// Duplicate a header under a new name
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderRenameDuplicateConfig {
    /// Source header name
    pub name: String,
    /// New name for the duplicate
    pub rename: String,
    /// Default value if source is missing
    #[serde(default)]
    pub default: Option<String>,
}
