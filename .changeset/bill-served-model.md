---
"synapse": patch
---

Bill the model that served a request, and read the resolved API key where it is stored

- Failover and cascade escalation charged, metered, and reported usage against the model resolved before the request was sent, so a request that failed over to a pricier equivalent, or escalated to a stronger model, was billed at the cheaper model's price. The credit wallet or allowance charge, the Aether usage events, the dashboard usage report, and the response cache entry now all record the provider and model that actually served the response, streamed or not
- The managed spend check is priced on the most expensive model the request can reach (the resolved model, its failover alternatives, and for a cascade the escalation model and its alternatives), so it never under-estimates a request that fails over or escalates. A managed request is never failed over or escalated to a model the gateway cannot price, since serving it would bill nothing
- The auth middleware stored the resolved API key as `Arc<ResolvedKey>` while the LLM layer read `ResolvedKey`, so the lookup always missed: dashboard usage reports were never sent for LLM requests, and the response cache scope never included the API key. Both sides now read and write it through one accessor in `synapse-auth`. Plan-based margins for LLM usage now come from the billing identity, as they already did for every other modality
