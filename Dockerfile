# Trovato production runtime image.
# Multi-stage: compiles kernel + WASM plugins, copies into slim runtime.
# For development, use .devcontainer/ instead (full Rust toolchain).

# ---- Build stage ----
FROM rust:1-bookworm AS builder

WORKDIR /usr/src/trovato

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add wasm32-wasip1

COPY . .

RUN cargo build --release --bin trovato

RUN cargo build --target wasm32-wasip1 --release \
    -p blog -p trovato_search -p categories -p comments \
    -p block_editor -p ritrovo_importer -p ritrovo_cfp \
    -p ritrovo_access -p ritrovo_notify -p ritrovo_translate \
    -p audit_log -p content_locking -p image_styles \
    -p locale -p media -p oauth2 -p redirects \
    -p scheduled_publishing -p webhooks \
    -p content_translation -p config_translation \
    -p trovato_ai

# ---- Runtime stage ----
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /usr/src/trovato/target/release/trovato .

# Assemble plugin directories: WASM binary + metadata + migrations
COPY --from=builder /usr/src/trovato/target/wasm32-wasip1/release/*.wasm /tmp/wasm/
COPY --from=builder /usr/src/trovato/plugins/ /tmp/plugin-src/
RUN for wasm in /tmp/wasm/*.wasm; do \
      name=$(basename "$wasm" .wasm); \
      mkdir -p "plugins/$name"; \
      cp "$wasm" "plugins/$name/"; \
    done && \
    for dir in /tmp/plugin-src/*/; do \
      name=$(basename "$dir"); \
      mkdir -p "plugins/$name"; \
      cp -f "$dir"/*.info.toml "plugins/$name/" 2>/dev/null || true; \
      [ -d "$dir/migrations" ] && cp -r "$dir/migrations" "plugins/$name/" || true; \
    done && \
    rm -rf /tmp/wasm /tmp/plugin-src

COPY templates/ templates/
COPY static/ static/
COPY docs/tutorial/config/ docs/tutorial/config/

EXPOSE 3000

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s \
    CMD curl -f http://localhost:3000/health || exit 1

CMD ["./trovato"]
