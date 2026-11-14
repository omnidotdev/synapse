---
"synapse": patch
---

Scope the response cache to the caller, bill cache hits, and make caching opt-in

The response cache keyed entries on the request alone and was consulted before provider resolution and the spend check, so:

- an identical request from another tenant was answered with the first tenant's cached response
- a cache hit skipped the credit and allowance checks and was never metered or charged, so an out-of-credits caller could replay cached responses free, and BYOK and managed responses were shared
- requests without a `temperature` were cached by default, so "regenerate" returned the same output for an hour

Now:

- every cache key is derived from a tenant scope (product, billing entity, managed or BYOK, and the API key), and a key cannot be built without one; requests with no identity are never cached
- the cache is consulted only after the same checks a live request gets, so a hit for a caller out of credits or allowance is refused with `402`
- a hit is billed exactly like a served request at the cached response's token usage: usage events, the credit deduct or allowance charge (each with a fresh idempotency key), and the dashboard usage report. The caller receives the full value of the response, so the cache is a latency and gateway cost optimization, never a free tier
- caching is opt-in per request with the `x-synapse-cache: true` header; temperature is no longer a signal, since current models are nondeterministic even at temperature 0 and products expect a fresh answer on regenerate. The `[cache]` config switch is unchanged
- entries use a new key version, so every entry written by the previous derivation is ignored
