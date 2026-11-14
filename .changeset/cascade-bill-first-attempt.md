---
"synapse": patch
---

Bill both attempts of an escalated cascade

- A cascade that escalated discarded the initial model's response, and its usage with it, so the upstream cost of that first attempt was never billed. It is now billed at the initial model's price (usage events, the credit wallet or allowance charge, and the dashboard usage report), alongside the escalated response at the escalation model's price
- The managed spend check for a cascade now covers both attempts: the most expensive model that may serve the first attempt plus the most expensive model that may serve the escalation
- A failed escalation serves the initial model's complete response (billed once) instead of returning an error, and escalation failover never falls back to the model whose answer was just rejected
