# Docker Development Guide

Trovato supports three Docker workflows depending on your needs.

## Quick Reference

| I want to... | Command | Needs Rust? |
|---|---|---|
| Develop Trovato natively | `docker compose up -d` | Yes |
| Develop without installing Rust | `docker compose --profile dev up -d` | No |
| Run a pre-built Trovato | `docker compose --profile full up -d` | No |

---

## Option 1: Native Development (Recommended)

For contributors with Rust installed. Docker provides only Postgres and Redis.

```bash
# Start database services
docker compose up -d

# Build and run Trovato natively (fast incremental builds)
cargo run --release --bin trovato

# Visit http://localhost:3000/install
```

This is the fastest workflow — native `cargo build` uses incremental compilation, so rebuilds after code changes take seconds.

**Prerequisites:** Rust 1.85+ (`rustup`), `wasm32-wasip1` target (`rustup target add wasm32-wasip1`)

---

## Option 2: Dev Container (No Rust Required)

For contributors who don't have Rust installed, or who want a reproducible development environment. The dev container provides the full Rust toolchain, clippy, rustfmt, WASM target, and database clients.

### Command Line

```bash
# Start Postgres, Redis, and the dev container
docker compose --profile dev up -d

# Open a shell in the dev container
docker compose exec dev bash

# Inside the container — full Rust toolchain available:
cargo build --release --bin trovato
cargo test --all
cargo clippy --all-targets -- -D warnings

# Run the server (accessible at http://localhost:3000 from your host)
cargo run --release --bin trovato
```

Your source code is mounted from the host at `/workspace`. Edit files with any editor on your host machine — changes are visible inside the container immediately.

Build artifacts are stored in Docker volumes (`cargo-registry`, `cargo-git`, `target-dir`) so they persist across container restarts and incremental builds stay fast.

### VS Code Dev Containers

If you use VS Code with the [Dev Containers extension](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-containers):

1. Open the Trovato repo in VS Code
2. VS Code detects `.devcontainer/devcontainer.json` and prompts "Reopen in Container"
3. Click it — VS Code builds the dev container and connects
4. The integrated terminal is inside the container with the full Rust toolchain
5. rust-analyzer, clippy, and formatting work automatically

### JetBrains (RustRover / IntelliJ)

JetBrains IDEs support Docker-based interpreters. Configure a Remote Rust Toolchain pointing to the `dev` service.

### First Build

The first `cargo build` inside the dev container downloads and compiles all dependencies. This takes several minutes. Subsequent builds use cached artifacts and are much faster.

```bash
# Inside the dev container:

# Build kernel
cargo build --release --bin trovato

# Build WASM plugins
cargo build --target wasm32-wasip1 --release -p ritrovo_importer

# Install a plugin
cargo run --release --bin trovato -- plugin install ritrovo_importer

# Run tests
cargo test -p trovato-kernel --lib

# Start the server
cargo run --release --bin trovato
# Visit http://localhost:3000/install from your host browser
```

### Working Through the Tutorial

The dev container has everything needed to follow the tutorial:

```bash
# Import configuration
cargo run --release --bin trovato -- config import docs/tutorial/config

# Connect to the database
psql $DATABASE_URL

# Check Redis
redis-cli -u $REDIS_URL ping
```

---

## Option 3: Pre-built Runtime

For evaluators who just want to see Trovato running without building anything.

```bash
# Pull the nightly image and start everything (fast — no compilation)
docker compose --profile full up -d

# Visit http://localhost:3000/install
```

This pulls the `ghcr.io/jeremyandrews/trovato:nightly` image, which is built automatically from `main` on every push. The image includes the compiled kernel, all WASM plugins, templates, and static assets.

To build locally instead of pulling the nightly:

```bash
# Build from source (first time takes ~15 minutes for Rust compilation)
docker compose --profile full up --build -d
```

### Nightly vs Release Images

| Tag | When Built | Use For |
|---|---|---|
| `nightly` | Every push to `main` | Preview latest features, testing |
| `nightly-abc1234` | Every push to `main` | Pin to a specific commit |
| `1.0.0`, `1.0`, `1` | On version tags | Production deployments |
| `latest` | On version tags | Production (latest stable) |

All images are published to `ghcr.io/jeremyandrews/trovato`.

---

## Troubleshooting

### Port 5432 already in use

A local PostgreSQL is already running on port 5432. Either stop it or change the port mapping in `docker-compose.yml`.

### Port 3000 already in use

Another process is using port 3000. Kill it or change the port mapping. If you're switching between native and Docker development, make sure to stop one before starting the other.

### Slow builds in dev container

The first build downloads and compiles all dependencies. Subsequent builds are fast because the `target-dir`, `cargo-registry`, and `cargo-git` volumes persist between container restarts. If you need to reset:

```bash
# Remove build caches (forces full rebuild)
docker volume rm trovato_target-dir trovato_cargo-registry trovato_cargo-git
```

### Database connection refused

Make sure Postgres is healthy before starting the server:

```bash
docker compose ps
# postgres should show "healthy"
```

### Redis connection refused

Same as above — check `docker compose ps` for Redis health.
