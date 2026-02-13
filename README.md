# Trovato

A content management system built in Rust, reimagining Drupal 6's mental model with modern foundations.

## What It Is

Trovato takes the core ideas that made Drupal 6 powerful—nodes, fields, views, hooks—and rebuilds them with:

- **Axum + Tokio** for async HTTP
- **PostgreSQL + JSONB** for flexible field storage without join complexity
- **WebAssembly plugins** running in per-request sandboxes via Wasmtime
- **Redis** for sessions, caching, and batch operations
- **Content staging** built into the schema from day one

## Key Features

### Plugin System
- **WASM Sandboxing**: Plugins run in isolated WebAssembly instances via Wasmtime with pooled allocation (~5µs instantiation)
- **Tap System**: Named hooks for content types, forms, access control, menus, permissions, and cron
- **Host Functions**: Structured API for database, caching, user context, logging, and inter-plugin calls
- **Secure Output**: Plugins return JSON render trees, kernel sanitizes and renders HTML

### Content Management
- **Dynamic Content Types**: Define types with custom fields via plugins, stored in JSONB
- **Field Types**: Text, long text, integer, float, boolean, date, email, file, entity reference
- **Revisions**: Full revision history with revert capability
- **Text Filters**: XSS-safe output with plain_text, filtered_html, and full_html formats
- **Staging**: Content stages built into schema for draft/live workflows

### Querying & Organization
- **Gather Query Engine**: Type-safe query building with 16+ filter operators and pagination
- **Categories & Tags**: DAG hierarchy with multiple parents per tag, recursive ancestor/descendant queries
- **Full-Text Search**: PostgreSQL tsvector with configurable field weights and ranking

### Forms & Admin
- **Form API**: Declarative definitions with validation, multi-step support, and AJAX
- **Theme Engine**: Tera templates with template suggestions and render element pipeline
- **Admin UI**: Content type management, field configuration, user administration

### Security & Auth
- **Authentication**: Argon2id password hashing, Redis sessions, account lockout
- **Access Control**: Role-based permissions with Deny > Grant > Neutral aggregation
- **CSRF Protection**: Token-based form protection
- **Rate Limiting**: Redis-backed distributed rate limiting

### Infrastructure
- **Cron & Queues**: Distributed locking via Redis, background task processing
- **File Management**: Upload handling with temporary file cleanup
- **Two-Tier Cache**: Moka L1 (in-memory) + Redis L2 with tag-based invalidation
- **Metrics**: Prometheus-compatible endpoint for monitoring
- **Batch Operations**: Long-running operations with progress tracking

## Architecture

No persistent state in the binary. PostgreSQL and Redis handle everything. Horizontal scaling works out of the box.

Plugins are untrusted code running in WASM sandboxes. They access data through host functions and return structured output that the kernel sanitizes and renders.

## Documentation

| Document | Description |
|----------|-------------|
| [Plugin Development Guide](docs/plugin-development.md) | Complete guide for plugin authors |
| [Plugin Quick Reference](docs/plugin-quick-reference.md) | Condensed API reference |
| [Architecture](docs/design/Architecture.md) | System architecture overview |
| [Content Model](docs/design/Design-Content-Model.md) | Content types, fields, and items |
| [Query Engine](docs/design/Design-Query-Engine.md) | Gather query building and execution |
| [Plugin System](docs/design/Design-Plugin-System.md) | Plugin loading, taps, and sandboxing |
| [Render & Theme](docs/design/Design-Render-Theme.md) | Rendering pipeline and theming |

## Quick Start

```bash
# Prerequisites: Rust, PostgreSQL, Redis

# Clone and build
git clone https://github.com/your-org/trovato.git
cd trovato
cargo build --release

# Set up environment
cp .env.example .env
# Edit .env with your database and Redis URLs

# Run migrations
sqlx database create
sqlx migrate run

# Start the server
cargo run -p trovato-kernel
```

## Building Plugins

```bash
# Install WASM target
rustup target add wasm32-wasip1

# Build a plugin
cargo build -p my_plugin --target wasm32-wasip1 --release
cp target/wasm32-wasip1/release/my_plugin.wasm plugins/my_plugin/
```

See the [Plugin Development Guide](docs/plugin-development.md) for complete documentation.

## Running Tests

```bash
# Run all tests
cargo test --all

# Run integration tests only
cargo test -p trovato-kernel --test integration_test
```

## Project Structure

```
trovato/
├── crates/
│   ├── kernel/          # HTTP server, plugin runtime, core services
│   └── plugin-sdk/      # SDK and proc macros for plugin development
├── plugins/             # Plugin WASM files and metadata
├── templates/           # Tera templates for HTML rendering
├── static/              # Static assets (CSS, JS)
├── migrations/          # SQLx database migrations
└── docs/                # Documentation
```

---

*This project is being developed with AI assistance.*
