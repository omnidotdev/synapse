# synapse

## 0.1.0

### Minor Changes

- [`571f3c0`](https://github.com/omnidotdev/synapse/commit/571f3c090290968cbf9df3200422f9e43f7786aa) Thanks [@coopbri](https://github.com/coopbri)! - Firing the synapse

  - Unified LLM routing across providers with automatic failover
  - MCP tool server aggregation and forwarding
  - Streaming and non-streaming completions
  - OpenAI-compatible API
  - Provider vault with encrypted key management
  - Intelligent routing strategies (threshold, cost, cascade, score, ONNX ML)
  - Circuit breaker failover with provider health tracking
  - Speech-to-text (Whisper, Deepgram) and text-to-speech (OpenAI, ElevenLabs)
  - Embeddings and image generation support
  - Input/output guardrails with PII detection
  - Response caching via Valkey
  - Distributed rate limiting with plan-aware enforcement
  - Billing integration with entitlement checks
  - OpenTelemetry tracing and metrics
