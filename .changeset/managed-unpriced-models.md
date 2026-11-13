---
"synapse": patch
---

Refuse managed requests for models the gateway cannot price, and price on the resolved model

Managed (Omni-key) usage was priced by looking up the raw requested model id in the price registry. The registry is keyed by bare model ids, so a provider-prefixed request (`anthropic/...`) or a virtual model (`auto`) never matched, priced at $0, and skipped the pre-request credit check, so a managed key with no balance was still served. A model with no price at all was also served and billed $0.

The pre-request estimate now prices the resolved provider and model, so the credit check runs for every managed request. A managed request for a model with no price is rejected with a 400 before the provider is called, and the error tells the caller to bring their own key to use that model. Bring-your-own-key requests are unaffected.
