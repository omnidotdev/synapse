---
"synapse": minor
---

Mark up embeddings, speech, and image usage like LLM usage

Embeddings, speech-to-text, text-to-speech, and image generation charged managed usage at raw upstream cost with no margin. They now charge base cost times the same margin policy the LLM path uses (the billed product's margin for product-scoped keys, the plan or provider margin for the gateway's own traffic), for both the pre-request credit check and the deduction. Image generation usage events record the charged amount. `MarginPolicy` moves to `synapse-core` (still re-exported from `synapse-llm`), and `BillingIdentity` carries the key's plan.
