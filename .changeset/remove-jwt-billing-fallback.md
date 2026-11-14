---
"synapse": patch
---

Remove the unreachable JWT billing identity fallback

The gateway authenticates by Synapse API key only: nothing validates a JWT or stores an authentication extension, and the API key middleware refuses any other bearer token, so the middleware that derived a billing identity from a JWT `sub` claim never ran. It is removed, and setting `[server.oauth]` now logs a boot warning that it has no effect
