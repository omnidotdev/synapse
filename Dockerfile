FROM rust:1.94-slim AS chef

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
# At this stage we don't really bother selecting anything specific, it's fast enough.
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
ENV CARGO_INCREMENTAL=0
COPY --from=planner /synapse/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY Cargo.lock Cargo.lock
COPY Cargo.toml Cargo.toml
COPY ./crates ./crates
COPY ./synapse ./synapse

RUN cargo build --release --bin synapse

#
# === Final image ===
#
FROM cgr.dev/chainguard/wolfi-base:latest

LABEL org.opencontainers.image.url='https://synapse.omni.dev' \
    org.opencontainers.image.documentation='https://synapse.omni.dev/docs' \
    org.opencontainers.image.source='https://github.com/omnidotdev/synapse-server' \
    org.opencontainers.image.vendor='Omni' \
    org.opencontainers.image.description='Omni Synapse - AI Router' \
    org.opencontainers.image.licenses='MIT'

WORKDIR /synapse

# Install curl for health checks, Node.js (npx) and uv (uvx) for MCP tool servers
RUN apk add --no-cache curl nodejs npm uv

# Create user and directories
# wolfi-base uses adduser from busybox
RUN adduser -D -u 1000 synapse && mkdir -p /data && chown synapse:synapse /data
USER synapse

COPY --from=builder /synapse/target/release/synapse /bin/synapse
COPY config/synapse.prod.toml /etc/synapse.toml

WORKDIR /data

ENTRYPOINT ["/bin/synapse"]
CMD ["--config", "/etc/synapse.toml", "--listen", "[::]:3000"]

EXPOSE 3000
