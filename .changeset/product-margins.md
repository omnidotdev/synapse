---
"synapse": minor
---

Mark up product-scoped usage by product, not by this gateway's plan names

`tier_margins` is keyed by Synapse's own plan names, so a product-scoped request (for example a Facet "pro" user) picked up Synapse's "pro" margin instead of its own product's retail markup. Margins now resolve per product: product-scoped traffic uses `[billing.product_margins]` for its app id, falling back to `default_product_margin` (2.0), and never uses tier or provider margins. Synapse's own traffic resolves exactly as before.

The billing section is now validated at startup (it previously never was): managed mode still requires a managed provider, and every margin must be a finite multiplier of at least 1.0, so a typo can never sell managed usage below cost.
