---
"synapse": minor
---

Serve managed AI billed to the pooled credit wallet only to paying customers

A free account costs nothing to create and the pooled credit wallet holds the free credit grants of every product, so free grants alone could be farmed across fresh accounts. A managed request billed to the wallet (every product outside `billing.included_allowance_products`, plus Synapse's own managed keys) is now refused before any provider call unless the entity is on a paid tier of the product (its `tier` entitlement is anything but `free`, so Omni platform workspaces on `team` stay eligible) or Aether's funding check reports it has bought credits. This applies to chat completions (streamed or not), messages, embeddings, image generation and edits, speech, and transcription.

The refusal is `402` with the distinct error type `payment_required` and the message "AI generation uses Omni credits. Buy credits or upgrade your plan to start.", separate from `insufficient_credits_error`, which a paying customer still gets when their balance is short.

The decision is cached per product and entity, five minutes for a paying customer (the balance check still runs on every request) and fifteen seconds otherwise, so a purchase or upgrade takes effect within seconds. A failure to reach Aether is never cached and follows `billing.wallet_fail_mode` (closed in production: `503 billing_unavailable_error`). Included allowance products and BYOK are not gated.

The gate is controlled by the new `billing.wallet_requires_paying_customer` (default `true`, set explicitly in production).
