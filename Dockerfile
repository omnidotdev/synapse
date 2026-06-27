FROM rust:1.94-slim@sha256:cf09adf8c3ebaba10779e5c23ff7fe4df4cccdab8a91f199b0c142c53fef3e1a AS chef

RUN apt-get update && apt-get install -y \
    curl \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY rust-toolchain.toml rust-toolchain.toml
RUN curl -L --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash && \
    cargo binstall --no-confirm cargo-chef sccache
ENV RUSTC_WRAPPER=sccache SCCACHE_DIR=/sccache

WORKDIR /synapse

FROM chef AS planner
# One-time cache-bust token: the registry buildcache held a stale cargo-chef
# recipe from before the workspace version bump (0.0.1 to 0.1.0), so the planner
# kept cache-hitting a 0.0.1 recipe. Bumping this forces a fresh recipe; it can
# be removed once the cache holds a healthy 0.1.0 recipe
ARG CHEF_CACHE_BUST=2026-06-27-2
RUN echo "chef cache bust ${CHEF_CACHE_BUST}"
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
ENV CARGO_INCREMENTAL=0
ARG CHEF_CACHE_BUST=2026-06-27-2
RUN echo "chef cache bust ${CHEF_CACHE_BUST}"
COPY --from=planner /synapse/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY Cargo.lock Cargo.lock
COPY Cargo.toml Cargo.toml
COPY ./crates ./crates
COPY ./synapse ./synapse

# cargo chef cook builds the workspace crates as stubs to cache dependencies,
# but leaves their fingerprints while the stub rlibs get cleaned. The final
# build then treats them as up to date and fails with "extern location does not
# exist". Drop the workspace fingerprints and artifacts so the real source is
# recompiled, while keeping the cooked third-party dependency cache
RUN rm -rf target/release/.fingerprint/synapse* target/release/deps/*synapse*

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
