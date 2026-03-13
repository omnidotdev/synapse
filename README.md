# Synapse

Unified AI router for LLM, embeddings, image generation, MCP, STT, and TTS provider management. Configure your API keys once, route all AI traffic through a single gateway.

## Features

- **Multi-Provider LLM Routing** -- OpenAI, Anthropic, Google, AWS Bedrock with automatic failover
- **Smart Model Selection** -- Threshold, cost, cascade, score, and ONNX-based routing strategies
- **Embeddings & Image Generation** -- Unified endpoints for embedding and image generation providers
- **MCP Aggregation** -- Aggregate Model Context Protocol servers through a single endpoint
- **STT/TTS** -- Speech-to-text (Whisper, Deepgram) and text-to-speech (OpenAI TTS, ElevenLabs)
- **Rate Limiting** -- In-memory and Redis-backed rate limiting with per-client policies
- **Authentication** -- API key validation, OAuth2/JWT with JWKS, CSRF protection
- **Billing Integration** -- Usage metering, managed margins, credit-based billing via Aether
- **Telemetry** -- OpenTelemetry metrics, tracing, and structured logs

## Quick Start

### Install

```bash
# Build from source
cargo build --release -p synapse

# Or use Docker
docker pull ghcr.io/omnidotdev/synapse:latest
```

### Configure

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

API keys support `{{ env.VAR }}` template syntax for environment variable substitution.

### Run

```bash
./target/release/synapse --config synapse.toml
```

### Send a Request

```bash
# OpenAI-compatible
curl http://localhost:6000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-sonnet-4-20250514",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'

# Anthropic-compatible
curl http://localhost:6000/v1/messages \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-sonnet-4-20250514",
    "max_tokens": 1024,
    "messages": [{"role": "user", "content": "Hello!"}]
  }'
```

Works with existing OpenAI and Anthropic SDKs -- just point `base_url` at your Synapse instance.

## Supported Providers

| Modality | Providers |
|----------|-----------|
| LLM | Anthropic, OpenAI, Google, AWS Bedrock |
| Embeddings | OpenAI |
| Image Generation | OpenAI (DALL-E) |
| STT | OpenAI Whisper, Deepgram |
| TTS | OpenAI TTS, ElevenLabs |
| MCP | Any STDIO, SSE, or StreamableHTTP server |

## Routing Strategies

Synapse can automatically select the best model for each request using virtual model names (`auto`, `fast`, `best`, `cheap`):

| Strategy | Description |
|----------|-------------|
| **Threshold** | Route by query complexity -- cheap models for simple queries, strong models for complex ones |
| **Cost** | Maximize quality within a per-request cost budget |
| **Cascade** | Try a cheap model first, escalate to a stronger model on low-confidence responses |
| **Score** | Multi-objective optimization balancing quality, cost, and latency |
| **ONNX** | ML-based classification using a trained ONNX model |

## Billing Modes

| Mode | Description |
|------|-------------|
| **BYOK** | Bring your own provider API keys -- no token metering |
| **Managed** | Synapse provides API keys and meters usage with configurable margins |
| **Credits** | Prepaid credit balance deducted per request based on model pricing |

## API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/chat/completions` | POST | LLM chat (OpenAI-compatible, streaming) |
| `/v1/messages` | POST | LLM chat (Anthropic-compatible, streaming) |
| `/v1/models` | GET | List available models |
| `/v1/embeddings` | POST | Generate embeddings |
| `/v1/images/generations` | POST | Generate images |
| `/v1/audio/transcriptions` | POST | Speech-to-text |
| `/v1/audio/speech` | POST | Text-to-speech |
| `/mcp/tools/list` | POST | List MCP tools |
| `/mcp/tools/call` | POST | Execute an MCP tool |
| `/health` | GET | Health check |

## Configuration

See the [full documentation](https://omni.dev/grid/synapse/configuration) for all configuration options including:

- Server settings (TLS, CORS, health endpoints)
- Provider configuration per modality
- Smart routing and model profiles
- Rate limiting (memory and Redis)
- Failover and circuit breaker
- Authentication (API keys, JWT/JWKS)
- Billing and usage metering
- OpenTelemetry exporters

## Architecture

```
┌──────────────────────────────┐
│           Synapse            │ :6000
│      Unified AI Router       │
├──────────────────────────────┤
│  ┌───┬───┬───┬───┬───┬───┐  │
│  │LLM│EMB│IMG│MCP│STT│TTS│  │
│  └─┬─┴─┬─┴─┬─┴─┬─┴─┬─┴─┬─┘  │
└────┼───┼───┼───┼───┼───┼────┘
     │   │   │   │   │   │
     ▼   ▼   ▼   ▼   ▼   ▼
 Anthropic OpenAI Google Bedrock
 Deepgram  ElevenLabs  MCP Servers
```

## Contributing

See the [contributing guide](/.github/CONTRIBUTING.md).

## License

The code in this repository is licensed under Apache 2.0, &copy; [Omni LLC](https://omni.dev). See [LICENSE.md](LICENSE.md) for more information.
