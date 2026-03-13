use synapse_config::McpAccessConfig;

use crate::error::McpError;

/// Tool-level access controller based on allow/deny lists
#[derive(Debug)]
pub struct AccessController {
    /// Per-server access rules keyed by server name
    rules: std::collections::HashMap<String, McpAccessConfig>,
}

impl AccessController {
    /// Build from MCP configuration
    pub fn new(servers: &indexmap::IndexMap<String, synapse_config::McpServerConfig>) -> Self {
        let rules = servers
            .iter()
            .filter_map(|(name, config)| config.access.as_ref().map(|access| (name.clone(), access.clone())))
            .collect();

        Self { rules }
    }

    /// Check whether a tool call is allowed
    ///
    /// Deny takes precedence over allow. If no rules are configured
    /// for the server, all tools are accessible.
    pub fn check(&self, server_name: &str, tool_name: &str) -> Result<(), McpError> {
        let Some(access) = self.rules.get(server_name) else {
            return Ok(());
        };

        // Deny list takes precedence
        if access.deny.iter().any(|d| d == tool_name) {
            return Err(McpError::AccessDenied {
                tool: format!("{server_name}__{tool_name}"),
            });
        }

        // If allow list is non-empty, tool must appear in it
        if !access.allow.is_empty() && !access.allow.iter().any(|a| a == tool_name) {
            return Err(McpError::AccessDenied {
                tool: format!("{server_name}__{tool_name}"),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config_with(allow: Vec<&str>, deny: Vec<&str>) -> McpAccessConfig {
        McpAccessConfig {
            allow: allow.into_iter().map(String::from).collect(),
            deny: deny.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn no_rules_allows_everything() {
        let ctrl = AccessController {
            rules: HashMap::default(),
        };
        assert!(ctrl.check("srv", "any_tool").is_ok());
    }

    #[test]
    fn deny_blocks_tool() {
        let mut rules = std::collections::HashMap::new();
        rules.insert("srv".to_string(), config_with(vec![], vec!["blocked"]));
        let ctrl = AccessController { rules };

        assert!(ctrl.check("srv", "blocked").is_err());
        assert!(ctrl.check("srv", "other").is_ok());
    }

    #[test]
    fn allow_restricts_to_listed() {
        let mut rules = std::collections::HashMap::new();
        rules.insert("srv".to_string(), config_with(vec!["allowed"], vec![]));
        let ctrl = AccessController { rules };

        assert!(ctrl.check("srv", "allowed").is_ok());
        assert!(ctrl.check("srv", "other").is_err());
    }

    #[test]
    fn deny_overrides_allow() {
        let mut rules = std::collections::HashMap::new();
        rules.insert("srv".to_string(), config_with(vec!["tool"], vec!["tool"]));
        let ctrl = AccessController { rules };

        assert!(ctrl.check("srv", "tool").is_err());
    }
}
