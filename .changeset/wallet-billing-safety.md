---
"synapse": minor
---

Make wallet-billed managed AI safe through Aether outages and overdrafts

Post-usage credit wallet debits are now sent as settlements (`settle: true`), so Aether records a served request's charge even when it takes the balance negative instead of refusing it and losing the charge. A debit Aether still refuses (an Aether that predates settlements) is logged once at error level with the app, entity, amount, idempotency key, and model needed to reconcile it, and is never retried.

Managed requests billed to the pooled credit wallet now fail closed when Aether is unreachable during the pre-request balance check: they are refused with `503 billing_unavailable_error` instead of being served unbilled. This is configured by the new `billing.wallet_fail_mode` (default `closed`, set explicitly in prod). `billing.fail_mode` still governs the entitlement gate and included allowance products, and BYOK is unaffected. An Aether 4xx response no longer counts toward opening the billing circuit breaker, so a run of refused requests cannot fail every wallet-billed request closed.

The pre-request cost estimate now counts image inputs, so vision requests are no longer underestimated: an inline image is priced at about width x height / 750 tokens after fitting a 2576 px long edge (Anthropic's documented formula, capped at 4784 tokens), and an image whose size cannot be read cheaply from its header is priced at that cap. Text estimation is unchanged.
