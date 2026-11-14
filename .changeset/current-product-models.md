---
"synapse": patch
---

Production model policies move to current models: beacon offers Claude Haiku 5.5 (default), Sonnet 5.5, and Opus 5.5; facet offers Sonnet 5.5 (default) and Opus 5.5, dropping the Sonnet 4.5 fallback now that facet-api's fallback is Sonnet 5.5; myfi offers Claude Haiku 5.5 only, which its `auto` requests resolve to, replacing Haiku 4.5 and Opus 4.6
