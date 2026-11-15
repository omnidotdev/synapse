---
"synapse": patch
---

Drop Kimi K2.5 from production pricing and the `frontier` failover group: NVIDIA's catalog replaced `moonshotai/kimi-k2.5` with `moonshotai/kimi-k2.6`, so every request for it 404ed upstream. `frontier` is now Claude Sonnet 5.5 and GPT-4o. The config guard lists it as retired
