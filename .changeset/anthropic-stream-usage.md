---
"synapse": patch
---

Bill streamed Anthropic usage and accept Claude 5 responses

Streamed Anthropic responses forwarded only the first event converted from each upstream SSE event. The final `message_delta` converts to both a stop reason and the usage that metering and credit deduction read, so streamed usage was dropped. When that usage carried output tokens only, the event also failed to parse, losing the stop reason too. Every converted event is now forwarded, and output-only usage takes its input count from `message_start`.

Claude 5 models think by default and reject parameters older models accept, so the gateway now:

- ignores `thinking`, `redacted_thinking`, and other unknown content blocks, deltas, and stream events instead of failing to parse the response
- drops `temperature` / `top_p` for models that reject sampling parameters (after the 4.6 generation, and Fable / Mythos)
- sends a forced tool choice (`required` or a named function) as `auto` where it is rejected (Opus and Sonnet from 5.5, Fable / Mythos from 5.1)
- sends `tool_choice: none` as `none` instead of `auto`
- maps the `refusal` stop reason to `content_filter`, and `stop_sequence` / `model_context_window_exceeded` to their finish reasons
