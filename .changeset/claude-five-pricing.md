---
"synapse": minor
---

Price Claude 5 models for managed billing and refuse unpriced managed models at startup

Adds managed prices for Claude Opus 5.5, Sonnet 5.5, Haiku 5.5, Opus 5, Sonnet 5, and Opus 4.8, and corrects Opus 4.6 ($5 / $25 per million tokens, was $15 / $75) and Haiku 4.5 ($1 / $5, was $0.80 / $4).

A model profile can declare `long_context_pricing` (`above_input_tokens`, `input_per_mtok`, `output_per_mtok`) for providers that charge a higher rate once a prompt is long; the whole request is priced at that rate, as the provider bills it. Haiku 5.5 uses it above 100K prompt tokens.

The gateway now refuses to start when a model of a managed provider has no input or output price (an omitted price used to read as $0 and bill nothing), or when its long-context rate is below its base rate. A test loads the production config through the same validation.
