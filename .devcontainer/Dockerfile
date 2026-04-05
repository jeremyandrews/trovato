# Trovato development container.
# Full Rust toolchain with all dependencies for building, testing, and running Trovato.
# Source code is mounted from the host — edits are instant.
# Cargo registry and target directory use named volumes for fast incremental builds.

FROM rust:1-bookworm

# System dependencies for compilation and development
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
    git curl \
    postgresql-client redis-tools \
    nodejs npm \
    && rm -rf /var/lib/apt/lists/*

# Rust toolchain components
RUN rustup component add clippy rustfmt \
    && rustup target add wasm32-wasip1

# Install cargo tools used in CI and development
RUN cargo install cargo-audit --locked 2>/dev/null || true

# Install Pagefind CLI for search index generation
RUN ARCH=$(dpkg --print-architecture) && \
    if [ "$ARCH" = "amd64" ]; then PF_ARCH="x86_64"; else PF_ARCH="aarch64"; fi && \
    curl -sL "https://github.com/CloudCannon/pagefind/releases/download/v1.3.0/pagefind-v1.3.0-${PF_ARCH}-unknown-linux-musl.tar.gz" \
    | tar xz -C /usr/local/bin pagefind && \
    chmod +x /usr/local/bin/pagefind

# Install Playwright for screenshot capture (used by tutorial verification)
RUN npm install -g playwright 2>/dev/null || true

WORKDIR /workspace

# Keep the container running for interactive use
CMD ["sleep", "infinity"]
