use std::sync::OnceLock;

use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use regex::Regex;
use serde::Deserialize;

/// Rule for transforming HTTP headers on outgoing requests
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HeaderRule {
    /// Forward a header from the incoming request
    Forward(HeaderForward),
    /// Insert a static header value
    Insert(HeaderInsert),
    /// Remove a header
    Remove(HeaderRemove),
    /// Duplicate a header under a new name
    RenameDuplicate(HeaderRenameDuplicate),
}

/// Forward a header from the incoming request, optionally renaming it
#[derive(Debug, Clone, Deserialize)]
pub struct HeaderForward {
    /// Header name or glob pattern to match
    pub name: NameOrPattern,
    /// Rename the header in the outgoing request
    #[serde(default)]
    pub rename: Option<ValidHeaderName>,
    /// Default value if the header is not present
    #[serde(default)]
    pub default: Option<ValidHeaderValue>,
}

/// Insert a static header value
#[derive(Debug, Clone, Deserialize)]
pub struct HeaderInsert {
    /// Header name to insert
    pub name: ValidHeaderName,
    /// Header value
    pub value: ValidHeaderValue,
}

/// Remove a header by name or pattern
#[derive(Debug, Clone, Deserialize)]
pub struct HeaderRemove {
    /// Header name or glob pattern
    pub name: NameOrPattern,
}

/// Duplicate a header: keep original and insert copy under new name
#[derive(Debug, Clone, Deserialize)]
pub struct HeaderRenameDuplicate {
    /// Source header name
    pub name: ValidHeaderName,
    /// New name for the duplicated header
    pub rename: ValidHeaderName,
    /// Default value if the source header is missing
    #[serde(default)]
    pub default: Option<ValidHeaderValue>,
}

/// Either a specific header name or a regex pattern
#[derive(Debug, Clone)]
pub enum NameOrPattern {
    /// Exact header name
    Name(ValidHeaderName),
    /// Regex pattern to match header names
    Pattern(HeaderPattern),
}

/// Wrapper for a validated HTTP header name
#[derive(Debug, Clone)]
pub struct ValidHeaderName(HeaderName);

impl ValidHeaderName {
    /// Create from a known-valid header name
    pub const fn new(name: HeaderName) -> Self {
        Self(name)
    }
}

impl AsRef<HeaderName> for ValidHeaderName {
    fn as_ref(&self) -> &HeaderName {
        &self.0
    }
}

/// Wrapper for a validated HTTP header value
#[derive(Debug, Clone)]
pub struct ValidHeaderValue(HeaderValue);

impl ValidHeaderValue {
    /// Create from a known-valid header value
    pub const fn new(value: HeaderValue) -> Self {
        Self(value)
    }
}

impl AsRef<HeaderValue> for ValidHeaderValue {
    fn as_ref(&self) -> &HeaderValue {
        &self.0
    }
}

/// Compiled regex pattern for matching header names
#[derive(Debug, Clone)]
pub struct HeaderPattern(pub Regex);

/// Headers that must never be forwarded to downstream providers
static DENY_LIST: OnceLock<[HeaderName; 21]> = OnceLock::new();

/// Get the header deny list
pub fn get_deny_list() -> &'static [HeaderName] {
    DENY_LIST.get_or_init(|| {
        [
            header::ACCEPT,
            header::ACCEPT_CHARSET,
            header::ACCEPT_ENCODING,
            header::ACCEPT_RANGES,
            header::CONTENT_LENGTH,
            header::CONTENT_TYPE,
            header::CONNECTION,
            HeaderName::from_static("keep-alive"),
            header::PROXY_AUTHENTICATE,
            header::PROXY_AUTHORIZATION,
            header::TE,
            header::TRAILER,
            header::TRANSFER_ENCODING,
            header::UPGRADE,
            header::ORIGIN,
            header::HOST,
            header::SEC_WEBSOCKET_VERSION,
            header::SEC_WEBSOCKET_KEY,
            header::SEC_WEBSOCKET_ACCEPT,
            header::SEC_WEBSOCKET_PROTOCOL,
            header::SEC_WEBSOCKET_EXTENSIONS,
        ]
    })
}

/// Check if a header name is in the deny list
pub fn is_header_denied(name: &HeaderName) -> bool {
    get_deny_list().contains(name)
}

/// Apply header rules to build a new header map for outgoing requests
///
/// # Arguments
/// * `incoming` - Headers from the incoming request
/// * `rules` - Rules to apply in order
pub fn apply_header_rules(incoming: &HeaderMap, rules: &[HeaderRule]) -> HeaderMap {
    let mut result = HeaderMap::new();

    if rules.is_empty() {
        return result;
    }

    for rule in rules {
        match rule {
            HeaderRule::Forward(forward) => {
                apply_forward(incoming, forward, &mut result);
            }
            HeaderRule::Insert(insert) => {
                result.insert(insert.name.0.clone(), insert.value.0.clone());
            }
            HeaderRule::Remove(remove) => {
                apply_remove(remove, &mut result);
            }
            HeaderRule::RenameDuplicate(dup) => {
                apply_rename_duplicate(incoming, dup, &mut result);
            }
        }
    }

    result
}

fn apply_forward(incoming: &HeaderMap, forward: &HeaderForward, result: &mut HeaderMap) {
    match &forward.name {
        NameOrPattern::Name(header_name) => {
            if is_header_denied(header_name.as_ref()) {
                return;
            }

            result.remove(header_name.as_ref());

            let value = incoming
                .get(header_name.as_ref())
                .cloned()
                .or_else(|| forward.default.as_ref().map(|d| d.0.clone()));

            if let Some(val) = value {
                if let Some(new_name) = &forward.rename {
                    result.insert(new_name.0.clone(), val);
                } else {
                    result.insert(header_name.0.clone(), val);
                }
            }
        }
        NameOrPattern::Pattern(pattern) => {
            let headers_to_forward: Vec<_> = incoming
                .keys()
                .filter(|k| !is_header_denied(k) && pattern.0.is_match(k.as_str()))
                .filter_map(|k| incoming.get(k).cloned().map(|v| (k.clone(), v)))
                .collect();

            for (original_name, value) in headers_to_forward {
                if let Some(new_name) = &forward.rename {
                    result.insert(new_name.0.clone(), value);
                } else {
                    result.insert(original_name, value);
                }
            }
        }
    }
}

fn apply_remove(remove: &HeaderRemove, result: &mut HeaderMap) {
    match &remove.name {
        NameOrPattern::Name(header_name) => {
            result.remove(header_name.as_ref());
        }
        NameOrPattern::Pattern(pattern) => {
            let to_remove: Vec<_> = result
                .keys()
                .filter(|key| pattern.0.is_match(key.as_str()))
                .cloned()
                .collect();

            for key in to_remove {
                result.remove(&key);
            }
        }
    }
}

fn apply_rename_duplicate(incoming: &HeaderMap, dup: &HeaderRenameDuplicate, result: &mut HeaderMap) {
    let value = incoming
        .get(dup.name.as_ref())
        .cloned()
        .or_else(|| dup.default.as_ref().map(|d| d.0.clone()));

    if let Some(val) = value {
        result.insert(dup.name.0.clone(), val.clone());
        result.insert(dup.rename.0.clone(), val);
    }
}

// Serde implementations for header types

impl<'de> Deserialize<'de> for ValidHeaderName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        HeaderName::try_from(s.as_str())
            .map(ValidHeaderName)
            .map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for ValidHeaderValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        HeaderValue::try_from(s.as_str())
            .map(ValidHeaderValue)
            .map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for NameOrPattern {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;

        // If the string contains regex metacharacters, treat as pattern
        if s.contains('*') || s.contains('?') || s.contains('[') || s.contains('(') {
            let regex = Regex::new(&s).map_err(|e| serde::de::Error::custom(format!("invalid pattern: {e}")))?;
            Ok(Self::Pattern(HeaderPattern(regex)))
        } else {
            let name = HeaderName::try_from(s.as_str()).map_err(serde::de::Error::custom)?;
            Ok(Self::Name(ValidHeaderName(name)))
        }
    }
}
