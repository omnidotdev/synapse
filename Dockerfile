FROM rust:1.94-slim@sha256:cf09adf8c3ebaba10779e5c23ff7fe4df4cccdab8a91f199b0c142c53fef3e1a AS builder

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
# build is reliable and matches local builds. Dependency caching can be
# reintroduced later via an sccache cache mount once that is understood
RUN cargo build --release --bin synapse

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

COPY --from=builder /synapse/target/release/synapse /bin/synapse
COPY config/synapse.prod.toml /etc/synapse.toml

WORKDIR /data

ENTRYPOINT ["/bin/synapse"]
CMD ["--config", "/etc/synapse.toml", "--listen", "[::]:3000"]

EXPOSE 3000
