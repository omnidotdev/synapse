---
"synapse": patch
---

Accept uploads up to the stated limits

- Speech-to-text read uploads up to 32 MiB but rebuilt the request without its extensions before parsing the multipart form, so axum's 2 MiB default applied and any audio file over 2 MiB failed. The route now carries a 32 MiB body limit and the limit survives into multipart parsing
- Chat completions and messages used axum's 2 MiB default, refusing ordinary multimodal requests with inline base64 images. Both now accept bodies up to 32 MiB, and the guardrail inspection buffer uses the same limit (it refused anything over 10 MiB)
