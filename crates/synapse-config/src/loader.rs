use std::path::Path;

use secrecy::ExposeSecret;

use crate::Config;

impl Config {
    /// Load configuration from a TOML file
    ///
    /// Reads the file, expands `{{ env.VAR }}` placeholders, then
    /// deserializes and validates the result.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read, environment variable
    /// expansion fails, TOML parsing fails, or validation fails
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read config file {}: {e}", path.display()))?;

        let expanded =
            crate::env::expand_env(&raw).map_err(|e| anyhow::anyhow!("config variable expansion failed: {e}"))?;

        let config: Self = toml::from_str(&expanded).map_err(|e| anyhow::anyhow!("failed to parse config: {e}"))?;

        config.validate()?;

        Ok(config)
    }

    /// Validate that the configuration is internally consistent
    ///
    /// # Errors
    ///
    /// Returns an error if required downstreams are missing or
    /// provider configurations are invalid
    pub fn validate(&self) -> anyhow::Result<()> {
        self.validate_has_downstreams()?;
        self.validate_llm_config()?;
        self.validate_mcp_config()?;
        self.validate_auth_config()?;
        Ok(())
    }

    /// Ensure at least one downstream service is configured
    fn validate_has_downstreams(&self) -> anyhow::Result<()> {
        let has_llm = !self.llm.providers.is_empty();
        let has_mcp = !self.mcp.servers.is_empty();
        let has_stt = !self.stt.providers.is_empty();
        let has_tts = !self.tts.providers.is_empty();

        if !has_llm && !has_mcp && !has_stt && !has_tts {
            anyhow::bail!(
                "at least one downstream must be configured (LLM provider, MCP server, STT provider, or TTS provider)"
            );
        }

        Ok(())
    }

    /// Validate LLM-specific configuration
    fn validate_llm_config(&self) -> anyhow::Result<()> {
        for (name, provider) in &self.llm.providers {
            // Validate model include/exclude patterns are valid regex
            for pattern in &provider.models.include {
                regex::Regex::new(pattern)
                    .map_err(|e| anyhow::anyhow!("invalid model include pattern for provider '{name}': {e}"))?;
            }
            for pattern in &provider.models.exclude {
                regex::Regex::new(pattern)
                    .map_err(|e| anyhow::anyhow!("invalid model exclude pattern for provider '{name}': {e}"))?;
            }
        }

        // Validate token rate limits require client identification
        if let Some(ref rate_limit) = self.server.rate_limit
            && rate_limit.tokens.is_some()
            && self.server.client_identification.is_none()
        {
            anyhow::bail!("token-based rate limiting requires client_identification to be configured");
        }

        Ok(())
    }

    /// Validate MCP-specific configuration
    fn validate_mcp_config(&self) -> anyhow::Result<()> {
        for (name, server) in &self.mcp.servers {
            if let Some(ref access) = server.access
                && !access.allow.is_empty()
                && !access.deny.is_empty()
            {
                anyhow::bail!("MCP server '{name}' cannot have both allow and deny lists");
            }
        }

        Ok(())
    }

    /// Validate auth configuration when auth is enabled
    fn validate_auth_config(&self) -> anyhow::Result<()> {
        let Some(ref auth) = self.auth else {
            return Ok(());
        };

        if !auth.enabled {
            return Ok(());
        }

        if auth.gateway_secret.expose_secret().is_empty() {
            anyhow::bail!("auth.gateway_secret must not be empty when auth is enabled");
        }

        if auth.cache_ttl_seconds == 0 {
            anyhow::bail!("auth.cache_ttl_seconds must be greater than 0");
        }

        if auth.cache_capacity > 1_000_000 {
            anyhow::bail!("auth.cache_capacity exceeds maximum of 1,000,000");
        }

        Ok(())
    }
}
