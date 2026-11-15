---
"synapse": patch
---

Production failover moves to current, priced models ordered so the first fallback always crosses vendors: `frontier` (Claude Sonnet 5.5, GPT-4o, Kimi K2.5), `premium` (Claude Opus 5.5, GPT-4o), and `fast` (Claude Haiku 5.5, GPT-4o mini), replacing Claude Opus 4.6, the deprecated Claude Sonnet 4.5, and Claude Haiku 4.5. A config guard keeps retired model ids out of the config and retired or deprecated ones out of failover, routing, and product policies
