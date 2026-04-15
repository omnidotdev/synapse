//! Model resolution and routing logic
//!
//! Resolves model names to provider + model pairs using the configuration
//! and discovered model lists.

use std::collections::HashMap;
use std::sync::Arc;

use regex::Regex;
use synapse_config::{EquivalenceGroup, LlmConfig};
use tokio::sync::RwLock;

use crate::error::LlmError;

/// Resolved target for a model request
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    /// Provider name (key in config)
    pub provider_name: String,
    /// Actual model identifier to send to the provider
    pub model_id: String,
    /// Whether the client explicitly specified the provider (e.g. "anthropic/claude-sonnet-4-20250514")
    pub explicit_provider: bool,
}

/// Routing-relevant model configuration extracted from a provider
#[derive(Debug, Clone, Default)]
struct ProviderModelConfig {
    /// Pre-compiled include patterns
    include: Vec<Regex>,
    /// Pre-compiled exclude patterns
    exclude: Vec<Regex>,
    /// Alias mappings: `actual_model_name` -> alias
    aliases: HashMap<String, String>,
    /// Reverse alias mappings: alias -> `actual_model_name`
    reverse_aliases: HashMap<String, String>,
}

/// Model routing table
pub struct ModelRouter {
    /// Routing-relevant config per provider
    providers: Vec<(String, ProviderModelConfig)>,
    /// Known models per provider, refreshed by discovery
    known_models: Arc<RwLock<HashMap<String, Vec<String>>>>,
}

impl ModelRouter {
    /// Create a new model router from configuration
    pub fn new(config: &LlmConfig) -> Self {
        let providers = config
            .providers
            .iter()
            .map(|(name, provider_config)| {
                let mut aliases = HashMap::new();
                let mut reverse_aliases = HashMap::new();

                for (actual_model, model_override) in &provider_config.models.overrides {
                    if let Some(alias) = &model_override.alias {
                        aliases.insert(actual_model.clone(), alias.clone());
                        reverse_aliases.insert(alias.clone(), actual_model.clone());
                    }
                }

                let include = provider_config.models.include.iter()
                    .filter_map(|p| Regex::new(p).ok())
                    .collect();
                let exclude = provider_config.models.exclude.iter()
                    .filter_map(|p| Regex::new(p).ok())
                    .collect();

                let model_config = ProviderModelConfig {
                    include,
                    exclude,
                    aliases,
                    reverse_aliases,
                };

                (name.clone(), model_config)
            })
            .collect();

        Self {
            providers,
            known_models: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get a reference to the shared known models map for discovery updates
    pub fn known_models(&self) -> Arc<RwLock<HashMap<String, Vec<String>>>> {
        Arc::clone(&self.known_models)
    }

    /// Resolve a model name to a provider and model identifier
    ///
    /// Supports two formats:
    /// - `provider_name/model_name` -- explicit provider selection
    /// - `model_name` -- searches all providers for a match
    ///
    /// # Errors
    ///
    /// Returns `LlmError::ModelNotFound` if no provider serves the model.
    /// Returns `LlmError::ProviderNotFound` if an explicit provider name is unknown.
    pub async fn resolve(&self, model: &str) -> Result<ResolvedModel, LlmError> {
        // Check for explicit provider/model format
        if let Some((provider_name, model_id)) = model.split_once('/') {
            let model_config = self
                .providers
                .iter()
                .find(|(name, _)| name == provider_name)
                .map(|(_, config)| config);

            let Some(model_config) = model_config else {
                return Err(LlmError::ProviderNotFound {
                    provider: provider_name.to_owned(),
                });
            };

            // Check aliases first
            let actual_model = resolve_alias(model_config, model_id);

            // Verify model passes include/exclude filters
            if !is_model_allowed(model_config, &actual_model) {
                return Err(LlmError::ModelNotFound {
                    model: model.to_owned(),
                });
            }

            return Ok(ResolvedModel {
                provider_name: provider_name.to_owned(),
                model_id: actual_model,
                explicit_provider: true,
            });
        }

        // Search all providers for the model
        let known_models = self.known_models.read().await;

        // First pass: check alias mappings
        for (provider_name, model_config) in &self.providers {
            let alias_match = resolve_alias(model_config, model);
            if alias_match != model && is_model_allowed(model_config, &alias_match) {
                return Ok(ResolvedModel {
                    provider_name: provider_name.clone(),
                    model_id: alias_match,
                    explicit_provider: false,
                });
            }
        }

        // Second pass: check known models from discovery
        {
            for (provider_name, models) in known_models.iter() {
                let model_config = self
                    .providers
                    .iter()
                    .find(|(name, _)| name == provider_name)
                    .map(|(_, config)| config);

                if let Some(model_config) = model_config
                    && models.iter().any(|m| m == model)
                    && is_model_allowed(model_config, model)
                {
                    return Ok(ResolvedModel {
                        provider_name: provider_name.clone(),
                        model_id: model.to_owned(),
                        explicit_provider: false,
                    });
                }
            }
        }
        drop(known_models);

        // Third pass: if no known models yet (discovery incomplete), try the
        // first provider that doesn't exclude the model. Once discovery has
        // populated model lists, unknown models should be rejected locally
        // rather than forwarded to an upstream that will 404.
        let has_discovered_models = self.known_models.read().await.values().any(|models| !models.is_empty());

        if !has_discovered_models {
            for (provider_name, model_config) in &self.providers {
                if is_model_allowed(model_config, model) {
                    return Ok(ResolvedModel {
                        provider_name: provider_name.clone(),
                        model_id: model.to_owned(),
                        explicit_provider: false,
                    });
                }
            }
        }

        Err(LlmError::ModelNotFound {
            model: model.to_owned(),
        })
    }

    /// Get all available models across all providers
    pub async fn list_models(&self) -> Vec<(String, String)> {
        let known_models = self.known_models.read().await;
        let mut result = Vec::new();

        for (provider_name, models) in known_models.iter() {
            let model_config = self
                .providers
                .iter()
                .find(|(name, _)| name == provider_name)
                .map(|(_, config)| config);

            if let Some(model_config) = model_config {
                for model in models {
                    if is_model_allowed(model_config, model) {
                        let display_name = model_config
                            .aliases
                            .get(model)
                            .cloned()
                            .unwrap_or_else(|| format!("{provider_name}/{model}"));
                        result.push((display_name, model.clone()));
                    }
                }
            }
        }
        drop(known_models);

        result
    }

    /// Find equivalent models for failover
    ///
    /// Given a "provider/model" pair and equivalence groups, returns
    /// alternative (provider, model) pairs excluding the original.
    pub fn find_equivalents(provider: &str, model: &str, groups: &[EquivalenceGroup]) -> Vec<(String, String)> {
        let key = format!("{provider}/{model}");
        let mut alternatives = Vec::new();

        for group in groups {
            if group.models.iter().any(|m| m == &key) {
                for entry in &group.models {
                    if entry != &key
                        && let Some((p, m)) = entry.split_once('/')
                    {
                        alternatives.push((p.to_owned(), m.to_owned()));
                    }
                }
            }
        }

        alternatives
    }
}

/// Check if a model passes the include/exclude filters
fn is_model_allowed(config: &ProviderModelConfig, model: &str) -> bool {
    // If include patterns are set, model must match at least one
    if !config.include.is_empty() {
        let matches_include = config.include.iter().any(|re| re.is_match(model));
        if !matches_include {
            return false;
        }
    }

    // Model must not match any exclude pattern
    !config.exclude.iter().any(|re| re.is_match(model))
}

/// Resolve a model alias to its actual name, or return the original
fn resolve_alias(config: &ProviderModelConfig, model: &str) -> String {
    config
        .reverse_aliases
        .get(model)
        .cloned()
        .unwrap_or_else(|| model.to_owned())
}
