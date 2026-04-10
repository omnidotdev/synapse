<div align="center">

# Synapse

Unified AI router for LLM, embeddings, image generation, MCP, STT, and TTS

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE.md)

[Website](https://synapse.omni.dev) | [Discord](https://discord.gg/omnidotdev) | [GitHub](https://github.com/omnidotdev/synapse)

</div>

## Overview

Synapse is a Rust gateway that routes AI traffic through a single endpoint. Configure your provider keys once, then point any OpenAI or Anthropic SDK at your Synapse instance. Synapse handles provider selection, automatic failover, smart routing, rate limiting, MCP aggregation, and usage metering across LLM, embedding, image, STT, and TTS providers.

## Installation

### From source

```bash
cargo install --path synapse
```

### Docker

```bash
docker pull ghcr.io/omnidotdev/synapse:latest
```

## Quick start

### 1. Configure

Create `synapse.toml`:

```toml
[server]
listen_address = "0.0.0.0:6000"

[llm.providers.anthropic]
type = "anthropic"
api_key = "{{ env.ANTHROPIC_API_KEY }}"

[llm.providers.openai]
type = "openai"
api_key = "{{ env.OPENAI_API_KEY }}"
```

API keys support `{{ env.VAR }}` template syntax for environment variable substitution. See `config/synapse.dev.toml` for a full development configuration.

### 2. Run

```bash
synapse --config synapse.toml
```

### 3. Send a request

```bash
# OpenAI-compatible
curl http://localhost:6000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-sonnet-4-20250514",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'
```

Works with existing OpenAI and Anthropic SDKs, just point `base_url` at your Synapse instance.

## Development

```bash
cargo build
cargo test
cargo clippy
cargo run -p synapse -- --config config/synapse.dev.toml
```

## Ecosystem

- **[Beacon](https://github.com/omnidotdev/beacon)** routes its LLM, STT/TTS, and tool execution through Synapse
- **[Omni CLI](https://github.com/omnidotdev/cli)** uses Synapse for model discovery and unified provider access
- **[Aether](https://github.com/omnidotdev/aether)** handles billing metering for managed and credit billing modes
- **[Warden](https://github.com/omnidotdev/warden)** enforces authorization on management endpoints

## License

The code in this repository is licensed under Apache 2.0, &copy; [Omni LLC](https://omni.dev). See [LICENSE.md](LICENSE.md) for more information.
