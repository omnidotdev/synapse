---
"synapse": minor
---

Pass thinking and reasoning effort through to providers

- `/v1/messages` accepts Anthropic `thinking` and `output_config` and sends them to Anthropic upstreams unchanged, so a caller can set adaptive thinking, the thinking display, `output_config.effort`, or structured output. Before, both were dropped and Claude models ran at their default effort
- `/v1/chat/completions` accepts `reasoning_effort` and sends it to OpenAI-compatible upstreams. The effort level is translated across formats: an OpenAI `reasoning_effort` reaching an Anthropic model becomes `output_config.effort` (`none` and `minimal` map to `low`), and an Anthropic `effort` reaching an OpenAI model becomes `reasoning_effort` (`max` maps to `xhigh`)
- Thinking tokens are billed: providers count them in output tokens, which the gateway meters and charges in full, streamed or not
