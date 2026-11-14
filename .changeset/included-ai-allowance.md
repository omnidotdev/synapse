---
"synapse": minor
---

Bound AI included in a product's tier by its fair-use allowance instead of the credit wallet

A product declared in `[billing.included_allowance_products]` (product to Aether usage meter, e.g. `myfi = "ai_credits"`) has every managed modality (chat, streaming, embeddings, speech, transcription, image generation) checked against that meter with the estimated cost in credits before the provider is called, and charged the actual cost in credits after. It never checks or deducts the pooled credit balance, and an exhausted allowance is refused with `402 allowance_exhausted_error`. Token and request meters record as before, and an unreachable Aether follows the configured `fail_mode` exactly like the credit check. Other products are unchanged. Prod declares myfi with a 2.0 margin.
