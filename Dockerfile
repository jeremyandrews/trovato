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
    -p trovato_blog -p trovato_media -p trovato_redirects \
    -p trovato_audit_log -p trovato_scheduled_publishing \
    -p trovato_content_locking -p trovato_webhooks \
    -p trovato_image_styles -p trovato_oauth2 \
    -p trovato_categories -p trovato_comments \
    -p trovato_locale -p trovato_content_translation \
    -p trovato_config_translation -p trovato_block_editor \
    -p trovato_search -p trovato_ai -p trovato_seo \
    -p trovato_page_builder -p trovato_captcha \
    -p trovato_series -p trovato_spam \
    -p trovato_book -p trovato_contact \
    -p goose

# ---- Runtime stage ----
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 curl \
    && rm -rf /var/lib/apt/lists/*

# Install Pagefind CLI for client-side search index generation.
# The cron task uses this to build the Pagefind index from published content.
RUN ARCH=$(dpkg --print-architecture) && \
    if [ "$ARCH" = "amd64" ]; then PF_ARCH="x86_64"; else PF_ARCH="aarch64"; fi && \
    curl -sL "https://github.com/CloudCannon/pagefind/releases/download/v1.3.0/pagefind-v1.3.0-${PF_ARCH}-unknown-linux-musl.tar.gz" \
    | tar xz -C /usr/local/bin pagefind && \
    chmod +x /usr/local/bin/pagefind

WORKDIR /app

COPY --from=builder /usr/src/trovato/target/release/trovato .

# Assemble plugin directories: WASM binary + metadata + migrations + the
# plugin's assets (page builder components, templates, static files).
# Directory names match crate names (trovato_ prefix convention). Only a plugin
# built above gets a directory: a manifest with no module behind it is a plugin
# the loader discovers and then cannot load (BL-122), so the in-tree test and
# reference plugins, which are not built here, are not shipped.
COPY --from=builder /usr/src/trovato/target/wasm32-wasip1/release/*.wasm /tmp/wasm/
COPY --from=builder /usr/src/trovato/plugins/ /tmp/plugin-src/
RUN for wasm in /tmp/wasm/*.wasm; do \
      name=$(basename "$wasm" .wasm); \
      src="/tmp/plugin-src/$name"; \
      [ -d "$src" ] || continue; \
      mkdir -p "plugins/$name"; \
      cp "$wasm" "plugins/$name/"; \
      cp -f "$src"/*.info.toml "plugins/$name/" 2>/dev/null || true; \
      for asset in migrations components templates static; do \
        [ -d "$src/$asset" ] && cp -r "$src/$asset" "plugins/$name/" || true; \
      done; \
    done && \
    rm -rf /tmp/wasm /tmp/plugin-src

COPY templates/ templates/
COPY static/ static/
COPY docs/tutorial/config/ docs/tutorial/config/

EXPOSE 3000

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s \
    CMD curl -f http://localhost:3000/health/live || exit 1

CMD ["./trovato"]
