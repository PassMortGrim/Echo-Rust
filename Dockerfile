# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS planner
WORKDIR /app
RUN cargo install cargo-chef --locked
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM rust:1-bookworm AS builder
WORKDIR /app
RUN cargo install cargo-chef --locked
COPY --from=planner /app/recipe.json recipe.json
# Montages de cache BuildKit : le cache par plateforme (amd64/arm64) rend
# les rebuilds multi-arch beaucoup plus rapides.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo chef cook --release --recipe-path recipe.json

COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --bin echo-rust

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git \
 && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --shell /bin/bash echo
USER echo
WORKDIR /home/echo

COPY --from=builder /app/target/release/echo-rust /usr/local/bin/echo-rust
COPY config.example.toml /home/echo/config.example.toml

ENV ECHO_SANDBOX__MODE=wasm \
    ECHO_VECTOR_DB__BACKEND=sqlite \
    ECHO_WASM__ENABLED=true

ENTRYPOINT ["/usr/local/bin/echo-rust"]