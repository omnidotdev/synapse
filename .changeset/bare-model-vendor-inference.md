---
"synapse": patch
---

Bare model ids resolve without relying on provider discovery: an id the gateway prices routes to its priced provider, and a first-party id routes to its vendor's provider by prefix (`claude-*` to Anthropic; `gpt-*`, `chatgpt-*`, `o1`/`o3`/`o4` to OpenAI; `gemini-*` to Google), preferring the provider named for the vendor over compatible ones. `claude-sonnet-5-5` and other current Claude ids no longer 404 for direct keys, and a prefix that names no provider (e.g. `moonshotai/kimi-k2.5`) resolves as part of the model id. The static Anthropic model list moves to the current lineup. Explicit `provider/model` requests still never fail over
