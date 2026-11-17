---
"synapse": patch
---

Production no longer configures the NVIDIA hosted API as an upstream: its terms allow trial use only. The managed gateway drops the `nvidia` provider and its managed key, so an explicit `nvidia/<model>` request now fails as an unknown provider. Self-hosted gateways can still point any OpenAI-compatible provider at their own endpoint. A config guard keeps trial-only upstreams out of the production config
