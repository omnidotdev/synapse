---
"synapse": minor
---

Add product-scoped billing so one gateway can meter multiple products

A provisioned API key can now carry its own `appId` and billing entity. When present, usage, credits, and entitlement checks resolve against that product (e.g. `facet`) and entity rather than the gateway's single configured app, letting downstream products delegate per-end-user AI billing to Synapse and Aether instead of keeping a local credit ledger. The Aether `appId` is now a per-call parameter on the billing client rather than a client-wide constant.

The entitlement, usage, and token-limit cache is now keyed by product as well as entity, so a decision cached for a user in one product can never answer a check for the same user in another. The entitlement-change webhook accepts an optional `app_id` (or `product_id`) to invalidate one product; without it, the change clears that entity in every product, so a sender that does not name the product never leaves a stale grant behind.

Fully backward compatible: a key without an `appId` falls back to the gateway's configured default app and bills its own user as a `user` entity, so existing single-product deployments are unchanged.
