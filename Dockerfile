# ---- Build stage ----
FROM rust:1-bookworm AS builder

WORKDIR /usr/src/trovato

# Install system dependencies needed for compilation (OpenSSL for sqlx/reqwest)
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy full source tree and build
COPY . .
RUN cargo build --release --bin trovato

# ---- Runtime stage ----
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the compiled binary
COPY --from=builder /usr/src/trovato/target/release/trovato .

# Copy pre-compiled WASM plugins
COPY plugins/ plugins/

# Copy templates, static assets, and default config
COPY templates/ templates/
COPY static/ static/
COPY .env.example .env.example

EXPOSE 3000

CMD ["./trovato"]
