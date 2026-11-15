---
"synapse": minor
---

`POST /v1/messages` accepts the Synapse API key in the `x-api-key` header as well as `Authorization: Bearer`, so the default Anthropic SDK works against the gateway unchanged. The key gets the same `synapse_` prefix check and resolution as a Bearer key, a Bearer key wins when both are sent, the header is dropped after authentication so it is never forwarded upstream, and every other endpoint still requires `Authorization: Bearer`
