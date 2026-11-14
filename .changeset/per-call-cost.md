---
"synapse": minor
---

Report what each LLM request cost

- Non-streamed `/v1/chat/completions` and `/v1/messages` responses carry `x-synapse-cost-usd` (retail amount charged, after the product margin, rounded up to $0.0001 like the charge itself), `x-synapse-credits` (the same in $0.01 credits), and `x-synapse-provider-cost-usd` (upstream provider cost before the margin)
- The same figures appear as a `synapse_cost` object (`cost_usd`, `credits`, `provider_cost_usd`) on the response's `usage`, and for streamed requests on the final usage: the usage chunk of an OpenAI-format stream and the `message_delta` usage of an Anthropic-format stream
- The cost covers every billed attempt: a cache hit is costed like a served request, failover reports the model that served, and an escalated cascade reports both attempts. BYOK requests report zero; requests without a billing identity report nothing
