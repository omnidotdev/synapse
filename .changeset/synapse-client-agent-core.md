---
"synapse": patch
---

Build `synapse-client` with the `agent-core` feature against Omni's agent-core

The workspace depended on the unrelated `agent-core` crate from crates.io (a TUI framework with no `provider`, `types`, or `error` modules), so `cargo build --all-features` failed. It now uses Omni's `agent-core` from `github.com/omnidotdev/agent-core`, the same source its consumers use, so the `LlmProvider` implementation unifies with theirs. `llm` stays locked at 1.3.7, since agent-core does not yet compile against 1.3.8
