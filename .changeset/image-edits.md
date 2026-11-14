---
"synapse": minor
---

Add image edits (`POST /v1/images/edits`) for generating with reference images

The endpoint takes the provider's multipart form (`model`, `prompt`, 1 to 16 `image[]` files, optional `mask`, `quality`, `size`, `n`, `input_fidelity`) and forwards every part to the provider unchanged, dropping only the provider prefix from `model`. Forms up to 64 MiB are accepted. The response has the same shape as generations, plus the provider's token `usage`.

Edits bill like generations. A managed request is priced before the provider is called and spend-checked against the credit wallet, or against the included allowance meter for products that include AI in their tier, then charged with the product margin. A managed edit for a model without a published price is refused with 400. BYOK requests are never charged.

Managed GPT Image prices, from the provider's published pricing (https://developers.openai.com/api/docs/pricing and the cost table in https://developers.openai.com/api/docs/guides/image-generation, read 2026-11-13), per million tokens:

| Model | Text input | Image input | Text output | Image output |
| --- | --- | --- | --- | --- |
| `gpt-image-1.5` | $5.00 | $8.00 | $10.00 | $32.00 |
| `gpt-image-1-mini` | $2.00 | $2.50 | (not published, billed as image) | $8.00 |

The charge comes from the token usage the provider reports, so reference images are billed as the image input tokens they actually cost. The pre-request estimate uses the published per-image output table (low / medium / high: $0.009 / $0.034 / $0.133 square and $0.013 / $0.05 / $0.2 portrait or landscape for `gpt-image-1.5`; $0.005 / $0.011 / $0.036 and $0.006 / $0.015 / $0.052 for `gpt-image-1-mini`) plus a worst-case input estimate per reference image (452 tokens, plus 6,240 at high input fidelity).

Image generations with `gpt-image-1.5` or `gpt-image-1-mini` now use the same prices and are charged from reported usage instead of the flat per-size table.

Image generations and edits also now:

- refuse a request with 402 (`insufficient_credits_error`) when the credit wallet cannot cover it, instead of 400
- serve a BYOK request on the caller's own provider key (registered for that provider, or forwarded in `X-Provider-API-Key`) and refuse it with 400 when there is none. Previously BYOK requests skipped billing but were served on the gateway's key, so they were free on Omni's account. Managed requests, and requests with no billing identity (billing off or self-hosted), keep the configured key
