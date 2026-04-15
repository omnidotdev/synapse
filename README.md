<div align="center">
  <h1 align="center">Synapse</h1>

[Website](https://synapse.omni.dev) | [Docs](https://docs.omni.dev/grid/synapse) | [Feedback](https://backfeed.omni.dev/workspaces/omni/projects/synapse) | [Discord](https://discord.gg/omnidotdev) | [X](https://x.com/omnidotdev)

</div>

**Synapse** is a unified AI router for LLM, embeddings, image generation, MCP, STT, and TTS. Configure your provider keys once, then point any OpenAI or Anthropic SDK at your Synapse instance. Synapse handles provider selection, automatic failover, smart routing, rate limiting, MCP aggregation, and usage metering across LLM, embedding, image, STT, and TTS providers.

## Installation

| Platform | Channel | Command / Link |
| --- | --- | --- |
| All | [GitHub Releases](https://github.com/omnidotdev/synapse/releases) | Download from releases page |
| All | [crates.io](https://crates.io/crates/synapse) | `cargo install synapse` |
| macOS / Linux | [Homebrew](https://github.com/omnidotdev/homebrew-tap/blob/master/Formula/synapse.rb) | `brew install omnidotdev/tap/synapse` |
| Arch Linux | [AUR](https://aur.archlinux.org/packages/omnidotdev-synapse) / [AUR (bin)](https://aur.archlinux.org/packages/omnidotdev-synapse-bin) | `paru -S omnidotdev-synapse` or `paru -S omnidotdev-synapse-bin` |
| Docker | [GHCR](https://ghcr.io/omnidotdev/synapse) | `docker pull ghcr.io/omnidotdev/synapse:latest` |

### Build from source

```bash
git clone https://github.com/omnidotdev/synapse
cd synapse
cargo build --release
# Binary will be at target/release/synapse
```

## Quick Start

### 1. Configure

Create `synapse.toml`:

```toml
[server]
listen_address = "0.0.0.0:6000"

[llm.providers.anthropic]
type = "anthropic"
api_key = "{{ env.ANTHROPIC_API_KEY }}"

[llm.providers.openai]
type = "openai"
api_key = "{{ env.OPENAI_API_KEY }}"
```

API keys support `{{ env.VAR }}` template syntax for environment variable substitution. See `config/synapse.dev.toml` for a full development configuration.

### 2. Run

```bash
synapse --config synapse.toml
```

### 3. Send a request

```bash
# OpenAI-compatible
curl http://localhost:6000/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-sonnet-4-20250514",
    "messages": [{"role": "user", "content": "Hello!"}]
  }'
```

Works with existing OpenAI and Anthropic SDKs, just point `base_url` at your Synapse instance.

## Development

### Prerequisites

- [Rust](https://rustup.rs) 1.94+
- [Bun](https://bun.sh) 1.0+

### Commands

```sh
cargo build          # Build
cargo run -- --help  # Run
cargo test           # Test
cargo clippy         # Lint
```

### Version Syncing

This project uses a dual-package setup (Rust crate + npm package) with automated version synchronization:

- **Source of truth**: `package.json` holds the canonical version, and is used for Changesets
- **Sync script**: `scripts/syncVersion.ts` propagates the version to `Cargo.toml`
- **Changesets**: Manages version bumps and changelog generation

The sync script runs automatically during the release process via the `version` npm script:

```sh
bun run version  # syncs `package.json` version -> `Cargo.toml`
```

### CI/CD

| Workflow | Trigger | Purpose |
|----------|---------|---------|
| `test.yml` | Push/PR to `master` | Runs fmt, clippy, and tests |
| `sync.yml` | PR to `master` | Validates version sync, fmt, clippy, test, build |
| `release.yml` | Push to `master` | Creates releases via Changesets, builds multi-platform binaries |

### Release Process

1. Create a changeset: `bun changeset`
2. Push to `master`
3. Changesets action creates a "Version Packages" PR
4. Merge the PR to trigger a release with binaries for:
   - `x86_64-unknown-linux-gnu`
   - `aarch64-unknown-linux-gnu`
   - `x86_64-apple-darwin`
   - `aarch64-apple-darwin`
5. **Manually** publish to crates.io: `cargo publish`

## Docker

Build and run with Docker:

```sh
docker build -t synapse .
docker run -p 6000:6000 synapse
```

## Ecosystem

- **[Beacon](https://github.com/omnidotdev/beacon)** routes its LLM, STT/TTS, and tool execution through Synapse
- **[Omni CLI](https://github.com/omnidotdev/cli)**: Agentic CLI for the Omni ecosystem
- **[Omni Terminal](https://github.com/omnidotdev/terminal)**: GPU-accelerated terminal emulator built to run everywhere
- **[Aether](https://github.com/omnidotdev/aether)** handles billing metering for managed and credit billing modes
- **[Warden](https://github.com/omnidotdev/warden)** enforces authorization on management endpoints

## License

The code in this repository is licensed under Apache 2.0, &copy; [Omni LLC](https://omni.dev). See [LICENSE.md](LICENSE.md) for more information.
