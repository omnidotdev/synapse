# syntax=docker/dockerfile:1
FROM rust:1.96-slim@sha256:31ee7fc65186be7e0e0ccb3f2ca305f14e4739e7642a1ae65753aa5d7b874523 AS builder

RUN apt-get update && apt-get install -y \
    curl \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

ENV CARGO_INCREMENTAL=0
WORKDIR /synapse

COPY rust-toolchain.toml rust-toolchain.toml
COPY Cargo.lock Cargo.lock
COPY Cargo.toml Cargo.toml
COPY ./crates ./crates
COPY ./synapse ./synapse

# Build straight from source. The previous cargo-chef setup left an inconsistent
# target state for the workspace crates (cooked stubs whose fingerprints
# survived but rlibs did not), which failed every build with "extern location
# does not exist" after the workspace version was bumped to 0.1.0. A direct
# build is reliable and matches local builds.
#
# Dependency caching is restored via BuildKit cache mounts on the cargo registry,
# git db, and target dir (cargo's own incremental caching, persisted on the
# buildkit worker across builds). The binary is copied out of the target cache
# mount since cache mounts are not part of the image layer
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/synapse/target \
    cargo build --release --bin synapse \
    && cp target/release/synapse /synapse/synapse-bin

#
# === Final image ===
#
FROM cgr.dev/chainguard/wolfi-base:latest@sha256:2f7a5c164eafbdbe46fe1d91bd1ab4c8cb5c2bdbd10641c3d61bd39962384cdb

LABEL org.opencontainers.image.url='https://synapse.omni.dev' \
    org.opencontainers.image.documentation='https://synapse.omni.dev/docs' \
    org.opencontainers.image.source='https://github.com/omnidotdev/synapse' \
    org.opencontainers.image.vendor='Omni' \
    org.opencontainers.image.description='Omni Synapse - AI Router' \
    org.opencontainers.image.licenses='Apache-2.0'

WORKDIR /synapse

# Install curl for health checks, Node.js (npx) and uv (uvx) for MCP tool servers
RUN apk add --no-cache curl nodejs npm uv

# Create user and directories.
# Pre-create npm and uv cache dirs as synapse:synapse so that npx/uvx can write
# to them at runtime. Without this, running npm/uv as a non-root user inside a
# Wolfi container fails with EACCES because the home dir subdirs are root-owned.
RUN adduser -D -u 1000 synapse \
    && mkdir -p /data /home/synapse/.npm /home/synapse/.cache/uv \
    && chown -R synapse:synapse /data /home/synapse
USER synapse

COPY --from=builder /synapse/synapse-bin /bin/synapse
COPY config/synapse.prod.toml /etc/synapse.toml

WORKDIR /data

ENTRYPOINT ["/bin/synapse"]
CMD ["--config", "/etc/synapse.toml", "--listen", "[::]:3000"]

EXPOSE 3000
