//! Model resolution and routing logic
//!
//! Resolves model names to provider + model pairs using the configuration
//! and discovered model lists.

use std::collections::HashMap;
use std::sync::Arc;

use regex::Regex;
use synapse_config::{EquivalenceGroup, LlmConfig, LlmProviderType};
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

/// Vendor a bare model id belongs to, inferred from its prefix
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vendor {
    Anthropic,
    Openai,
    Google,
}

impl Vendor {
    /// Infer the vendor from a bare model id (`claude-*` is Anthropic,
    /// `gpt-*` / `o1*` / `o3*` / `o4*` / `chatgpt-*` are `OpenAI`, `gemini-*` is Google)
    fn from_model(model: &str) -> Option<Self> {
        let model = model.to_ascii_lowercase();
        if model.starts_with("claude-") {
            return Some(Self::Anthropic);
        }
        if model.starts_with("gpt-")
            || model.starts_with("chatgpt-")
            || ["o1", "o3", "o4"]
                .iter()
                .any(|family| model == *family || model.starts_with(&format!("{family}-")))
        {
            return Some(Self::Openai);
        }
        if model.starts_with("gemini-") {
            return Some(Self::Google);
        }
        None
    }

    /// Provider name a vendor's own provider is conventionally configured as
    const fn canonical_name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::Google => "google",
        }
    }

    /// Whether a provider of this type can serve the vendor's first-party API
    const fn served_by(self, provider_type: &LlmProviderType) -> bool {
        matches!(
            (self, provider_type),
            (Self::Anthropic, LlmProviderType::Anthropic)
                | (Self::Openai, LlmProviderType::Openai)
                | (Self::Google, LlmProviderType::Google)
        )
    }
}

/// Model routing table
pub struct ModelRouter {
    /// Routing-relevant config per provider
    providers: Vec<(String, ProviderModelConfig)>,
    /// Provider type per provider name, for vendor inference
    provider_types: HashMap<String, LlmProviderType>,
    /// Priced model profiles as `(provider, model)`, the gateway's own catalog
    priced: Vec<(String, String)>,
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

                let include = provider_config
                    .models
                    .include
                    .iter()
                    .filter_map(|p| Regex::new(p).ok())
                    .collect();
                let exclude = provider_config
                    .models
                    .exclude
                    .iter()
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

        let provider_types = config
            .providers
            .iter()
            .map(|(name, provider_config)| (name.clone(), provider_config.provider_type.clone()))
            .collect();
        let priced = config
            .routing
            .models
            .iter()
            .map(|profile| (profile.provider.clone(), profile.model.clone()))
            .collect();

        Self {
            providers,
            provider_types,
            priced,
            known_models: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// The provider that serves a bare model id without discovery: the one a
    /// priced profile names for exactly this model, else the vendor's own
    /// provider inferred from the id prefix (the provider named for the
    /// vendor first, then any provider of the vendor's type), as long as its
    /// filters allow the model
    fn infer_provider(&self, model: &str) -> Option<String> {
        let allowed = |provider: &str| {
            self.providers
                .iter()
                .find(|(name, _)| name == provider)
                .is_some_and(|(_, config)| is_model_allowed(config, model))
        };

        let mut priced = self.priced.iter().filter(|(_, m)| m == model).map(|(p, _)| p);
        if let Some(provider) = priced.next()
            && priced.next().is_none()
            && allowed(provider)
        {
            return Some(provider.clone());
        }

        let vendor = Vendor::from_model(model)?;
        let canonical = vendor.canonical_name();
        let serves = |name: &str| {
            self.provider_types
                .get(name)
                .is_some_and(|provider_type| vendor.served_by(provider_type))
                && allowed(name)
        };
        if serves(canonical) {
            return Some(canonical.to_owned());
        }
        self.providers
            .iter()
            .map(|(name, _)| name)
            .find(|name| serves(name))
            .cloned()
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
                // A prefix that names no provider can be part of the model id
                // itself (e.g. `moonshotai/kimi-k2.5`, priced under nvidia)
                if let Some(provider) = self.infer_provider(model) {
                    return Ok(ResolvedModel {
                        provider_name: provider,
                        model_id: model.to_owned(),
                        explicit_provider: false,
                    });
                }
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

        // Third pass: a model the gateway prices, or a first-party id whose
        // vendor its prefix names (`claude-*`, `gpt-*`, ...). Discovery lists
        // can lag a new release or fail outright, and must never be the only
        // way a bare id resolves
        if let Some(provider_name) = self.infer_provider(model) {
            return Ok(ResolvedModel {
                provider_name,
                model_id: model.to_owned(),
                explicit_provider: false,
            });
        }

        // Fourth pass: if no known models yet (discovery incomplete), try the
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

#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use synapse_config::{LlmProviderConfig, ModelConfig, ModelProfileConfig};

    use super::*;

    fn provider(provider_type: LlmProviderType, include: &[&str]) -> LlmProviderConfig {
        LlmProviderConfig {
            provider_type,
            api_key: Some(SecretString::from("k")),
            base_url: None,
            models: ModelConfig {
                include: include.iter().map(|p| (*p).to_owned()).collect(),
                ..ModelConfig::default()
            },
            headers: Vec::new(),
            forward_authorization: false,
            rate_limit: None,
        }
    }

    fn profile(provider: &str, model: &str) -> ModelProfileConfig {
        ModelProfileConfig {
            provider: provider.to_owned(),
            model: model.to_owned(),
            display_name: None,
            context_window: 200_000,
            input_per_mtok: 1.0,
            output_per_mtok: 2.0,
            quality: 0.9,
            capabilities: synapse_config::ModelCapabilities::default(),
            long_context_pricing: None,
        }
    }

    /// The production shape: an OpenAI-compatible nvidia provider sits beside
    /// the first-party openai and anthropic providers
    fn prod_like() -> LlmConfig {
        let mut config = LlmConfig::default();
        config
            .providers
            .insert("openai".to_owned(), provider(LlmProviderType::Openai, &[]));
        config
            .providers
            .insert("anthropic".to_owned(), provider(LlmProviderType::Anthropic, &[]));
        config
            .providers
            .insert("nvidia".to_owned(), provider(LlmProviderType::Openai, &[]));
        config.routing.models = vec![
            profile("anthropic", "claude-sonnet-5-5"),
            profile("openai", "gpt-4o-mini"),
            profile("nvidia", "moonshotai/kimi-k2.5"),
        ];
        config
    }

    async fn discovered(router: &ModelRouter, provider: &str, models: &[&str]) {
        router
            .known_models()
            .write()
            .await
            .insert(provider.to_owned(), models.iter().map(|m| (*m).to_owned()).collect());
    }

    async fn resolve(router: &ModelRouter, model: &str) -> (String, String, bool) {
        let resolved = router.resolve(model).await.unwrap();
        (resolved.provider_name, resolved.model_id, resolved.explicit_provider)
    }

    #[tokio::test]
    async fn bare_ids_route_to_their_vendor_when_discovery_misses_them() {
        let router = ModelRouter::new(&prod_like());
        // Discovery ran (OpenAI listed its models) but knows no current Claude
        discovered(&router, "openai", &["gpt-4o", "gpt-4o-mini"]).await;
        discovered(&router, "anthropic", &["claude-opus-4-6"]).await;

        for model in ["claude-sonnet-5-5", "claude-haiku-5-5", "claude-opus-5-5"] {
            assert_eq!(
                resolve(&router, model).await,
                ("anthropic".into(), model.into(), false),
                "{model}"
            );
        }
        for model in ["gpt-4o-mini", "gpt-5", "o3-mini", "o4-mini"] {
            assert_eq!(
                resolve(&router, model).await,
                ("openai".into(), model.into(), false),
                "{model}"
            );
        }
    }

    #[tokio::test]
    async fn vendor_inference_prefers_the_first_party_provider_over_compatible_ones() {
        let mut config = prod_like();
        // A compatible provider listed before the first-party one
        config
            .providers
            .shift_insert(0, "proxy".to_owned(), provider(LlmProviderType::Openai, &[]));
        let router = ModelRouter::new(&config);
        discovered(&router, "anthropic", &["claude-opus-4-6"]).await;
        assert_eq!(resolve(&router, "gpt-4.1").await.0, "openai");
    }

    #[tokio::test]
    async fn vendor_inference_falls_back_to_any_provider_of_the_vendor_type() {
        let mut config = LlmConfig::default();
        config
            .providers
            .insert("claude".to_owned(), provider(LlmProviderType::Anthropic, &[]));
        config
            .providers
            .insert("oai".to_owned(), provider(LlmProviderType::Openai, &[]));
        let router = ModelRouter::new(&config);
        discovered(&router, "oai", &["gpt-4o"]).await;
        assert_eq!(resolve(&router, "claude-haiku-5-5").await.0, "claude");
    }

    #[tokio::test]
    async fn priced_ids_route_to_their_priced_provider() {
        let router = ModelRouter::new(&prod_like());
        discovered(&router, "openai", &["gpt-4o"]).await;
        // `moonshotai` names no provider, so the whole id is a bare model
        assert_eq!(
            resolve(&router, "moonshotai/kimi-k2.5").await,
            ("nvidia".into(), "moonshotai/kimi-k2.5".into(), false)
        );
    }

    #[tokio::test]
    async fn explicit_provider_ids_stay_explicit() {
        let router = ModelRouter::new(&prod_like());
        assert_eq!(
            resolve(&router, "openai/gpt-4o-mini").await,
            ("openai".into(), "gpt-4o-mini".into(), true)
        );
        assert_eq!(
            resolve(&router, "nvidia/moonshotai/kimi-k2.5").await,
            ("nvidia".into(), "moonshotai/kimi-k2.5".into(), true)
        );
    }

    #[tokio::test]
    async fn unknown_bare_ids_and_providers_are_still_refused() {
        let router = ModelRouter::new(&prod_like());
        discovered(&router, "openai", &["gpt-4o"]).await;
        assert!(matches!(
            router.resolve("llama-3-70b").await,
            Err(LlmError::ModelNotFound { .. })
        ));
        assert!(matches!(
            router.resolve("nope/whatever").await,
            Err(LlmError::ProviderNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn provider_filters_still_apply_to_inferred_vendors() {
        let mut config = LlmConfig::default();
        config.providers.insert(
            "anthropic".to_owned(),
            provider(LlmProviderType::Anthropic, &["^claude-haiku-"]),
        );
        config
            .providers
            .insert("openai".to_owned(), provider(LlmProviderType::Openai, &[]));
        let router = ModelRouter::new(&config);
        discovered(&router, "openai", &["gpt-4o"]).await;
        assert_eq!(resolve(&router, "claude-haiku-5-5").await.0, "anthropic");
        assert!(router.resolve("claude-opus-5-5").await.is_err());
    }

    #[test]
    fn vendor_prefixes() {
        assert_eq!(Vendor::from_model("claude-sonnet-5-5"), Some(Vendor::Anthropic));
        assert_eq!(Vendor::from_model("gpt-4o-mini"), Some(Vendor::Openai));
        assert_eq!(Vendor::from_model("o3-mini"), Some(Vendor::Openai));
        assert_eq!(Vendor::from_model("o3"), Some(Vendor::Openai));
        assert_eq!(Vendor::from_model("gemini-2.5-pro"), Some(Vendor::Google));
        assert_eq!(Vendor::from_model("omni-model"), None);
        assert_eq!(Vendor::from_model("llama-3"), None);
    }
}
