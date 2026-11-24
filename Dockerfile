# Trovato production runtime image.
# Multi-stage: compiles kernel + WASM plugins, copies into slim runtime.
# For development, use .devcontainer/ instead (full Rust toolchain).

# ---- Build stages ----
#
# The dependency build is its own layer, so a change to the workspace's own
# source does not recompile the ~400 crates underneath it. `cargo chef
# prepare` reduces the workspace to its manifests and lockfile (workspace
# member versions masked, so a version bump is not a dependency change), and
# `cargo chef cook` builds exactly the dependencies `cargo build --release
# --bin trovato` needs. The layer is only rebuilt when Cargo.toml or
# Cargo.lock change, or the base image moves.
#
# Measured on the CI Cluster job, which builds this file on every pull request:
# the dependencies were 8.4 of the 17.7 minutes the release build took (run
# 45316035626). That job caches the `cook` stage below on each push to main
# and reads it on pull requests, so a branch that does not change dependencies
# starts from the cooked layer.
FROM rust:1-bookworm AS chef

WORKDIR /usr/src/trovato

# The pinned toolchain, before anything is built. Without it here the cooked
# dependencies were built by the image's own stable Rust, the `COPY . .` below
# then brought in rust-toolchain.toml, rustup switched to the pinned version,
# and every crate was compiled again by the other compiler.
COPY rust-toolchain.toml .

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add wasm32-wasip1

# Pinned so that the recipe format, and with it the cooked layer's cache key,
# only moves when this line does.
RUN cargo install cargo-chef --version 0.1.78 --locked

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# The dependencies alone, as their own target: the Cluster job in ci.yml
# builds and caches this stage by name.
FROM chef AS cook
COPY --from=planner /usr/src/trovato/recipe.json recipe.json
RUN cargo chef cook --release --bin trovato --recipe-path recipe.json

FROM cook AS builder
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
