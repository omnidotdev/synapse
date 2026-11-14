---
"synapse": patch
---

Deliver every usage report to synapse-api, for every billed modality, at sub-cent precision

- Usage reports never landed: a key outside a workspace serialized `workspaceId: null`, which synapse-api's body schema rejects with a 422, and the reporter only treated transport errors as failures, so every rejected batch was silently counted as delivered. An absent workspace is now omitted, each event carries an `eventId` (retries record once) and its served time `occurredAt`, and the reporter checks the response: a rejected batch (4xx) is logged as an error and dropped, a transient failure (5xx, 401, 408, 429, unreachable) is re-queued
- Every modality reports what it served through one request extension (`synapse_core::UsageReportSink`, installed by the auth middleware for the resolved key): completions, embeddings, speech-to-text, text-to-speech, image generation, and image edits, each with its `modality` (the charge's Aether reference type). Embeddings, speech, and image spend was charged in Aether but never reported, so it could not reconcile
- The reported cost is exactly what Aether was charged (rounded up to the ledger's $0.0001), in cents to two places, instead of an estimate rounded to whole cents
- Speech-to-text, text-to-speech, embeddings, and image generation resolve their provider margin by the vendor the provider calls (`openai` for Whisper), like their BYOK key, instead of the configured provider name. Production prices are unchanged: the `openai` managed-provider margin is 1.0, the same as the fallback the unmatched name `whisper` got
- Image generation no longer accepts a provider key in the `X-Provider-API-Key` header for BYOK requests; like every other endpoint, BYOK runs only on the key registered with `setProviderKey`
