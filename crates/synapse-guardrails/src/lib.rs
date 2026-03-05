//! Configurable content filtering and guardrails for LLM requests
//!
//! Provides a rule engine that checks input content against configurable
//! blocklists, regex patterns, token limits, and PII detection patterns.
//! Rules can either block requests (returning 403) or warn (log and allow).

use std::sync::OnceLock;

use regex::Regex;
use thiserror::Error;

/// Guardrails errors
#[derive(Debug, Error)]
pub enum GuardrailError {
    /// Content was blocked by a rule
    #[error("content blocked: {rule} - {reason}")]
    Blocked {
        /// Name of the rule that triggered
        rule: String,
        /// Human-readable reason
        reason: String,
    },
    /// Invalid regex pattern in configuration
    #[error("invalid regex pattern: {0}")]
    InvalidPattern(String),
}

/// Action to take when a rule matches
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Block the request with a 403 response
    Block,
    /// Log a warning but allow the request through
    Warn,
}

impl Default for Action {
    fn default() -> Self {
        Self::Block
    }
}

/// A single guardrail rule
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Rule {
    /// Block or warn on specific keywords (case-insensitive substring match)
    KeywordBlocklist {
        /// Rule name for logging
        name: String,
        /// Keywords to match against
        keywords: Vec<String>,
        /// Action when matched
        #[serde(default)]
        action: Action,
    },
    /// Block or warn on regex pattern matches
    RegexPattern {
        /// Rule name for logging
        name: String,
        /// Regex pattern to match
        pattern: String,
        /// Action when matched
        #[serde(default)]
        action: Action,
    },
    /// Enforce maximum input token count
    MaxInputTokens {
        /// Rule name for logging
        name: String,
        /// Maximum token estimate (words * 1.3 heuristic)
        limit: usize,
        /// Action when exceeded
        #[serde(default)]
        action: Action,
    },
    /// Detect common PII patterns
    Pii {
        /// Rule name for logging
        name: String,
        /// PII types to detect
        #[serde(default)]
        detect: Vec<PiiType>,
        /// Action when detected
        #[serde(default)]
        action: Action,
    },
}

/// Types of PII to detect
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PiiType {
    /// US Social Security numbers (XXX-XX-XXXX)
    Ssn,
    /// Credit card numbers (13-19 digit sequences)
    CreditCard,
    /// Email addresses
    Email,
    /// US phone numbers
    Phone,
}

/// Compiled guardrails engine
pub struct GuardrailEngine {
    rules: Vec<CompiledRule>,
}

/// A rule with pre-compiled regex patterns
struct CompiledRule {
    name: String,
    action: Action,
    matcher: RuleMatcher,
}

enum RuleMatcher {
    Keywords(Vec<String>),
    Regex(Regex),
    MaxTokens(usize),
    Pii(Vec<CompiledPii>),
}

struct CompiledPii {
    pii_type: PiiType,
    pattern: Regex,
}

/// Result of checking content against guardrails
#[derive(Debug)]
pub struct CheckResult {
    /// Whether the content was blocked
    pub blocked: bool,
    /// Warnings generated (rule name + reason)
    pub warnings: Vec<(String, String)>,
    /// Block reason if blocked
    pub block_reason: Option<(String, String)>,
}

impl GuardrailEngine {
    /// Compile a set of rules into an engine
    ///
    /// # Errors
    ///
    /// Returns an error if any regex pattern is invalid
    pub fn new(rules: &[Rule]) -> Result<Self, GuardrailError> {
        let compiled = rules.iter().map(compile_rule).collect::<Result<Vec<_>, _>>()?;

        Ok(Self { rules: compiled })
    }

    /// Check text content against all rules
    #[must_use]
    pub fn check(&self, content: &str) -> CheckResult {
        let lower = content.to_lowercase();
        let mut warnings = Vec::new();
        let mut block_reason = None;

        for rule in &self.rules {
            if let Some(reason) = rule.matcher.matches(content, &lower) {
                match rule.action {
                    Action::Block => {
                        tracing::warn!(rule = %rule.name, %reason, "guardrail blocked request");
                        block_reason = Some((rule.name.clone(), reason));
                        // Stop on first block
                        break;
                    }
                    Action::Warn => {
                        tracing::warn!(rule = %rule.name, %reason, "guardrail warning");
                        warnings.push((rule.name.clone(), reason));
                    }
                }
            }
        }

        CheckResult {
            blocked: block_reason.is_some(),
            warnings,
            block_reason,
        }
    }

    /// Returns true if no rules are configured
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

impl RuleMatcher {
    fn matches(&self, original: &str, lowered: &str) -> Option<String> {
        match self {
            Self::Keywords(keywords) => {
                for keyword in keywords {
                    if lowered.contains(&keyword.to_lowercase()) {
                        return Some(format!("matched keyword: {keyword}"));
                    }
                }
                None
            }
            Self::Regex(pattern) => {
                if pattern.is_match(original) {
                    return Some(format!("matched pattern: {}", pattern.as_str()));
                }
                None
            }
            Self::MaxTokens(limit) => {
                // Rough token estimate: word count * 1.3
                let word_count = original.split_whitespace().count();
                // Token estimate: ~1.3 tokens per word
                let estimated_tokens = word_count + word_count * 3 / 10;
                if estimated_tokens > *limit {
                    return Some(format!("estimated {estimated_tokens} tokens exceeds limit of {limit}"));
                }
                None
            }
            Self::Pii(detectors) => {
                for detector in detectors {
                    if detector.pattern.is_match(original) {
                        return Some(format!("detected {:?} pattern", detector.pii_type));
                    }
                }
                None
            }
        }
    }
}

fn compile_rule(rule: &Rule) -> Result<CompiledRule, GuardrailError> {
    match rule {
        Rule::KeywordBlocklist { name, keywords, action } => Ok(CompiledRule {
            name: name.clone(),
            action: action.clone(),
            matcher: RuleMatcher::Keywords(keywords.clone()),
        }),
        Rule::RegexPattern { name, pattern, action } => {
            let compiled =
                Regex::new(pattern).map_err(|e| GuardrailError::InvalidPattern(format!("{pattern}: {e}")))?;
            Ok(CompiledRule {
                name: name.clone(),
                action: action.clone(),
                matcher: RuleMatcher::Regex(compiled),
            })
        }
        Rule::MaxInputTokens { name, limit, action } => Ok(CompiledRule {
            name: name.clone(),
            action: action.clone(),
            matcher: RuleMatcher::MaxTokens(*limit),
        }),
        Rule::Pii { name, detect, action } => {
            let detectors = detect
                .iter()
                .map(|pii_type| {
                    let pattern = pii_regex(pii_type);
                    Ok(CompiledPii {
                        pii_type: pii_type.clone(),
                        pattern,
                    })
                })
                .collect::<Result<Vec<_>, GuardrailError>>()?;

            Ok(CompiledRule {
                name: name.clone(),
                action: action.clone(),
                matcher: RuleMatcher::Pii(detectors),
            })
        }
    }
}

/// Get the compiled regex for a PII type
fn pii_regex(pii_type: &PiiType) -> Regex {
    match pii_type {
        PiiType::Ssn => ssn_regex().clone(),
        PiiType::CreditCard => credit_card_regex().clone(),
        PiiType::Email => email_regex().clone(),
        PiiType::Phone => phone_regex().clone(),
    }
}

fn ssn_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").expect("valid SSN regex"))
}

fn credit_card_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(?:\d[ -]*?){13,19}\b").expect("valid credit card regex"))
}

fn email_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Z|a-z]{2,}\b").expect("valid email regex"))
}

fn phone_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(?:\+?1[-.\s]?)?\(?[2-9]\d{2}\)?[-.\s]?\d{3}[-.\s]?\d{4}\b").expect("valid phone regex")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_blocklist_matches() {
        let engine = GuardrailEngine::new(&[Rule::KeywordBlocklist {
            name: "test".to_owned(),
            keywords: vec!["hack".to_owned(), "exploit".to_owned()],
            action: Action::Block,
        }])
        .unwrap();

        let result = engine.check("How to hack a system");
        assert!(result.blocked);

        let result = engine.check("How to build a system");
        assert!(!result.blocked);
    }

    #[test]
    fn keyword_case_insensitive() {
        let engine = GuardrailEngine::new(&[Rule::KeywordBlocklist {
            name: "test".to_owned(),
            keywords: vec!["BLOCKED".to_owned()],
            action: Action::Block,
        }])
        .unwrap();

        let result = engine.check("this is blocked content");
        assert!(result.blocked);
    }

    #[test]
    fn regex_pattern_matches() {
        let engine = GuardrailEngine::new(&[Rule::RegexPattern {
            name: "test".to_owned(),
            pattern: r"DROP\s+TABLE".to_owned(),
            action: Action::Block,
        }])
        .unwrap();

        let result = engine.check("Run this SQL: DROP TABLE users");
        assert!(result.blocked);

        let result = engine.check("What is a database table?");
        assert!(!result.blocked);
    }

    #[test]
    fn max_input_tokens() {
        let engine = GuardrailEngine::new(&[Rule::MaxInputTokens {
            name: "test".to_owned(),
            limit: 10,
            action: Action::Block,
        }])
        .unwrap();

        let result = engine.check("short message");
        assert!(!result.blocked);

        let long = "word ".repeat(100);
        let result = engine.check(&long);
        assert!(result.blocked);
    }

    #[test]
    fn pii_ssn_detection() {
        let engine = GuardrailEngine::new(&[Rule::Pii {
            name: "pii".to_owned(),
            detect: vec![PiiType::Ssn],
            action: Action::Block,
        }])
        .unwrap();

        let result = engine.check("My SSN is 123-45-6789");
        assert!(result.blocked);

        let result = engine.check("The number is 12345");
        assert!(!result.blocked);
    }

    #[test]
    fn pii_email_detection() {
        let engine = GuardrailEngine::new(&[Rule::Pii {
            name: "pii".to_owned(),
            detect: vec![PiiType::Email],
            action: Action::Block,
        }])
        .unwrap();

        let result = engine.check("Contact me at user@example.com");
        assert!(result.blocked);

        let result = engine.check("No email here");
        assert!(!result.blocked);
    }

    #[test]
    fn warn_action_does_not_block() {
        let engine = GuardrailEngine::new(&[Rule::KeywordBlocklist {
            name: "soft-filter".to_owned(),
            keywords: vec!["flagged".to_owned()],
            action: Action::Warn,
        }])
        .unwrap();

        let result = engine.check("this is flagged content");
        assert!(!result.blocked);
        assert_eq!(result.warnings.len(), 1);
    }

    #[test]
    fn multiple_rules_first_block_wins() {
        let engine = GuardrailEngine::new(&[
            Rule::KeywordBlocklist {
                name: "warn-rule".to_owned(),
                keywords: vec!["alert".to_owned()],
                action: Action::Warn,
            },
            Rule::KeywordBlocklist {
                name: "block-rule".to_owned(),
                keywords: vec!["alert".to_owned()],
                action: Action::Block,
            },
        ])
        .unwrap();

        let result = engine.check("trigger alert");
        assert!(result.blocked);
        assert_eq!(result.warnings.len(), 1);
    }

    #[test]
    fn invalid_regex_returns_error() {
        let result = GuardrailEngine::new(&[Rule::RegexPattern {
            name: "bad".to_owned(),
            pattern: r"[invalid".to_owned(),
            action: Action::Block,
        }]);
        assert!(result.is_err());
    }
}
