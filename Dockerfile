# syntax=docker/dockerfile:1.7

FROM rust:1.85-bookworm AS builder
WORKDIR /src

# Build deps for the `openssl` vendored feature.
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config perl make \
 && rm -rf /var/lib/apt/lists/*

# Cache dependencies in a separate layer.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
 && echo "fn main() {}" > src/main.rs \
 && cargo build --release \
 && rm -rf src target/release/deps/httpbandwidthspeedtester*

COPY src ./src
RUN cargo build --release \
 && strip target/release/httpbandwidthspeedtester

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/httpbandwidthspeedtester /usr/local/bin/httpbandwidthspeedtester
ENTRYPOINT ["/usr/local/bin/httpbandwidthspeedtester"]
