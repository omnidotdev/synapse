//! Background model list discovery and refresh
//!
//! Periodically fetches available models from each configured provider
//! and updates the shared routing table.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use secrecy::ExposeSecret;
use synapse_config::{LlmConfig, LlmProviderConfig, LlmProviderType};
use tokio::sync::RwLock;

/// Default refresh interval for model discovery
const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(300);

/// Start the background model discovery task
///
/// This spawns a tokio task that periodically fetches model lists from
/// each configured provider and updates the shared model map.
#[allow(clippy::implicit_hasher)]
pub fn start_discovery(config: LlmConfig, known_models: Arc<RwLock<HashMap<String, Vec<String>>>>) {
    tokio::spawn(async move {
        let client = Client::new();

        // Do an initial fetch immediately
        refresh_all(&client, &config, &known_models).await;

        // Then refresh periodically
        let mut interval = tokio::time::interval(DEFAULT_REFRESH_INTERVAL);
        loop {
            interval.tick().await;
            refresh_all(&client, &config, &known_models).await;
        }
    });
}

/// Refresh models from all providers
async fn refresh_all(client: &Client, config: &LlmConfig, known_models: &Arc<RwLock<HashMap<String, Vec<String>>>>) {
    for (name, provider_config) in &config.providers {
        match fetch_models(client, name, provider_config).await {
            Ok(models) => {
                tracing::debug!(
                    provider = %name,
                    count = models.len(),
                    "discovered models"
                );
                let mut map = known_models.write().await;
                map.insert(name.clone(), models);
            }
            Err(e) => {
                tracing::warn!(
                    provider = %name,
                    error = %e,
                    "failed to discover models"
                );
            }
        }
    }
}

/// Fetch the model list from a single provider
async fn fetch_models(client: &Client, _name: &str, config: &LlmProviderConfig) -> Result<Vec<String>, String> {
    match &config.provider_type {
        LlmProviderType::Openai => fetch_openai_models(client, config).await,
        LlmProviderType::Anthropic => Ok(static_anthropic_models()),
        LlmProviderType::Google => fetch_google_models(client, config).await,
        LlmProviderType::Bedrock(bedrock_config) => fetch_bedrock_models(bedrock_config).await,
    }
}

/// Fetch models from an OpenAI-compatible endpoint
async fn fetch_openai_models(client: &Client, config: &LlmProviderConfig) -> Result<Vec<String>, String> {
    let base_url = config.base_url.as_ref().map_or_else(
        || "https://api.openai.com/v1".to_owned(),
        |u| u.as_str().trim_end_matches('/').to_owned(),
    );

    let url = format!("{base_url}/models");

    let mut builder = client.get(&url);
    if let Some(api_key) = &config.api_key {
        builder = builder.bearer_auth(api_key.expose_secret());
    }

    let response = builder.send().await.map_err(|e| format!("request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("status {}", response.status()));
    }

    let body: crate::protocol::openai::OpenAiModelList =
        response.json().await.map_err(|e| format!("parse error: {e}"))?;

    Ok(body.data.into_iter().map(|m| m.id).collect())
}

/// Static list of known Anthropic models
///
/// Anthropic doesn't provide a models list endpoint, so we maintain
/// a static list of commonly available models
fn static_anthropic_models() -> Vec<String> {
    vec![
        "claude-sonnet-4-20250514".to_owned(),
        "claude-3-5-sonnet-20241022".to_owned(),
        "claude-3-5-haiku-20241022".to_owned(),
        "claude-3-opus-20240229".to_owned(),
        "claude-3-sonnet-20240229".to_owned(),
        "claude-3-haiku-20240307".to_owned(),
    ]
}

/// Fetch models from the Google Generative Language API
async fn fetch_google_models(client: &Client, config: &LlmProviderConfig) -> Result<Vec<String>, String> {
    let base_url = config.base_url.as_ref().map_or_else(
        || "https://generativelanguage.googleapis.com/v1beta".to_owned(),
        |u| u.as_str().trim_end_matches('/').to_owned(),
    );

    let mut url = format!("{base_url}/models");
    if let Some(api_key) = &config.api_key {
        use std::fmt::Write;
        let _ = write!(url, "?key={}", api_key.expose_secret());
    }

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("status {}", response.status()));
    }

    let body: crate::protocol::google::GoogleModelList =
        response.json().await.map_err(|e| format!("parse error: {e}"))?;

    Ok(body
        .models
        .into_iter()
        .filter(|m| {
            m.supported_generation_methods
                .iter()
                .any(|method| method == "generateContent")
        })
        .map(|m| {
            // Strip "models/" prefix
            m.name.strip_prefix("models/").unwrap_or(&m.name).to_owned()
        })
        .collect())
}

/// Fetch models from AWS Bedrock
async fn fetch_bedrock_models(bedrock_config: &synapse_config::BedrockConfig) -> Result<Vec<String>, String> {
    let mut aws_config_builder = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(bedrock_config.region.clone()));

    if let (Some(access_key), Some(secret_key)) = (&bedrock_config.access_key_id, &bedrock_config.secret_access_key) {
        let credentials = aws_credential_types::Credentials::new(
            access_key.expose_secret(),
            secret_key.expose_secret(),
            None,
            None,
            "synapse-discovery",
        );
        aws_config_builder = aws_config_builder.credentials_provider(credentials);
    }

    let aws_config = aws_config_builder.load().await;
    let bedrock_client = aws_sdk_bedrock::Client::new(&aws_config);

    let output = bedrock_client
        .list_foundation_models()
        .send()
        .await
        .map_err(|e| format!("bedrock list models failed: {e}"))?;

    let models = output
        .model_summaries()
        .iter()
        .map(|m| m.model_id().to_owned())
        .collect();

    Ok(models)
}
