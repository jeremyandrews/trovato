# Trovato

A content management system built in Rust, reimagining Drupal 6's mental model with modern foundations.

## What It Is

Trovato takes the core ideas that made Drupal 6 powerful -- nodes, fields, views, hooks -- and rebuilds them with:

- **Axum + Tokio** for async HTTP
- **PostgreSQL + JSONB** for flexible field storage without join complexity
- **WebAssembly plugins** running in per-request sandboxes via Wasmtime
- **Redis** for sessions, caching, and batch operations
- **Content staging** built into the schema from day one

## Key Features

### Plugin System
- **WASM Sandboxing**: Plugins run in isolated WebAssembly instances via Wasmtime with pooled allocation (~5us instantiation)
- **Tap System**: Named hooks for content types, forms, access control, menus, permissions, and cron
- **Host Functions**: Structured API for database, caching, user context, logging, and inter-plugin calls
- **Secure Output**: Plugins return JSON render trees, kernel sanitizes and renders HTML
- **Plugin CLI**: `trovato plugin list|install|migrate|enable|disable` for lifecycle management

### Content Management
- **Dynamic Content Types**: Define types with custom fields via plugins, stored in JSONB
- **Field Types**: Text, long text, integer, float, boolean, date, email, file, entity reference
- **Revisions**: Full revision history with revert capability
- **Text Filters**: XSS-safe output with plain_text, filtered_html, and full_html formats
- **Staging**: Content stages built into schema for draft/live workflows
- **Stage Hierarchy**: Parent/child stage chains with upstream publishing and content overlay inheritance
- **Stage-Aware Menus & Aliases**: Path aliases and menu links resolve per active stage, with conflict detection
- **Scheduled Publishing**: Future publish/unpublish dates via `field_publish_on` / `field_unpublish_on`
- **Content Locking**: Pessimistic locking prevents concurrent editing with heartbeat and break support

### Querying & Organization
- **Gather Query Engine**: Type-safe query building with 16+ filter operators, pagination, and exposed filters
- **Gather Admin UI**: Visual query builder with live preview, relationship editor, display configuration, and cloning
- **Performance Guardrails**: Statement timeout, join depth limits, max items per page enforcement on gather queries
- **Categories & Tags**: DAG hierarchy with multiple parents per tag, recursive ancestor/descendant queries
- **Full-Text Search**: PostgreSQL tsvector with configurable field weights and ranking, integrated as gather filter operator
- **URL Aliases**: Clean URLs with automatic path alias resolution middleware
- **Redirects**: URL redirect management with automatic alias-change tracking and loop detection

### Forms & Admin
- **Form API**: Declarative definitions with validation, multi-step support, and AJAX
- **Theme Engine**: Tera templates with template suggestions and render element pipeline
- **Admin UI**: Content type management, field configuration, user administration
- **Config Export/Import**: `trovato config export|import` for YAML-based configuration management

### Block Editor
- **Editor.js Integration**: Rich content editing with Editor.js field widget for block-based content
- **8 Standard Block Types**: Paragraph, heading, image, list, quote, code, delimiter, embed
- **Block Type Registry**: Extensible registry with JSON Schema validation per block type
- **Server-Side Rendering**: Semantic HTML output with Tera templates per block type
- **HTML Sanitization**: Ammonia-based XSS prevention on all text block output
- **Code Highlighting**: Syntax highlighting via syntect with auto-detected language
- **Embed Whitelist**: YouTube/Vimeo URL validation with responsive iframe rendering
- **Image Upload**: Multipart upload endpoint with MIME validation for Editor.js image tool

### Media & Images
- **File Management**: Upload handling with temporary file cleanup and managed file tracking
- **Media Entities**: Media content type wrapping file_managed with revision tracking and stage awareness
- **Image Styles**: On-demand derivative generation with configurable effect chains (scale, crop, resize, desaturate)

### Security & Auth
- **Authentication**: Argon2id password hashing, Redis sessions, account lockout
- **OAuth2 Provider**: Authorization code (with PKCE), client credentials, and refresh token grants with JWT tokens
- **Bearer Auth**: JWT middleware with token type verification and Redis revocation blocklist
- **Access Control**: Role-based permissions with Deny > Grant > Neutral aggregation
- **CSRF Protection**: Token-based form protection
- **Rate Limiting**: Redis-backed distributed sliding-window rate limiting
- **SSRF Prevention**: DNS rebinding mitigation, private/CGNAT/benchmarking IP blocking, port restrictions on outbound webhooks
- **Path Traversal Protection**: Lexical path normalization and component-based validation
- **Audit Logging**: Tracks content CRUD, authentication events, and permission changes

### Internationalization
- **Language Negotiation**: Resolves active language via session, URL prefix, Accept-Language, or site default
- **Interface Translation**: Gettext .po file import, in-memory translation cache, Tera `t()` function
- **Content Translation**: Field-level translation overlay on base items with language fallback
- **Config Translation**: Translatable configuration entity overlay for active language

### Webhooks & Integration
- **Event Dispatch**: Content events trigger matching webhook deliveries
- **HMAC-SHA256 Signatures**: Signed payloads with `X-Webhook-Signature` header
- **AES-256-GCM Encryption**: Webhook secrets encrypted at rest
- **Retry with Backoff**: Exponential retry at 1min, 5min, 30min, 2hr intervals

### Infrastructure
- **Cron & Queues**: Distributed locking via Redis, background task processing
- **Two-Tier Cache**: Moka L1 (in-memory) + Redis L2 with tag-based invalidation
- **Metrics**: Prometheus-compatible endpoint for monitoring
- **Batch Operations**: Long-running operations with progress tracking
- **Health Check**: `/health` endpoint for load balancers

## Architecture

No persistent state in the binary. PostgreSQL and Redis handle everything. Horizontal scaling works out of the box.

Plugins are untrusted code running in WASM sandboxes. They access data through host functions and return structured output that the kernel sanitizes and renders.

### Middleware Pipeline

Requests flow through: tracing -> sessions -> CORS -> bearer auth -> API token auth -> install check -> language negotiation -> redirect check -> path alias resolution -> route handlers.

## Standard Plugins

Trovato ships with 17 plugins compiled to WebAssembly:

### Content Type Plugins
| Plugin | Content Types | Description |
|--------|--------------|-------------|
| `blog` | blog | Blog posts with body and tags |
| `media` | media | File upload wrapper with alt text, caption, credit |
| `argus` | 7 types | News intelligence: articles, stories, topics, feeds, entities, reactions, discussions |
| `netgrasp` | 6 types | Network monitoring: devices, persons, events, presence tracking |
| `goose` | 5 types | Load testing data: test runs, scenarios, endpoint results, sites, comparisons |

### Feature Plugins
| Plugin | Description |
|--------|-------------|
| `categories` | Category vocabulary and term management with DAG hierarchy |
| `comments` | Threaded comment system with moderation and approval workflows |
| `audit_log` | Audit trail for content, auth, and permission changes |
| `content_locking` | Pessimistic locking to prevent concurrent editing |
| `scheduled_publishing` | Schedule items for future publish and unpublish |
| `webhooks` | Event-driven webhook dispatch with HMAC signatures and retry |
| `image_styles` | On-demand image derivatives with configurable effect chains |
| `oauth2` | OAuth2 authorization server with JWT, PKCE, and token rotation |
| `redirects` | URL redirect management with automatic alias-change tracking |

### Internationalization Plugins
| Plugin | Description |
|--------|-------------|
| `locale` | Interface string translation with .po file import |
| `content_translation` | Field-level content translation with language overlay |
| `config_translation` | Configuration entity translation for active language |

## Quick Start

### Prerequisites

- Rust (stable)
- PostgreSQL 15+
- Redis 7+

### Using Docker Compose (recommended for dependencies)

```bash
# Start PostgreSQL and Redis
docker compose up -d

# Clone and build
git clone https://github.com/jeremyandrews/trovato.git
cd trovato
cargo build --release

# Set up environment
cp .env.example .env
# Edit .env if needed (defaults work with docker-compose)

# Start the server
cargo run --release
```

### Manual Setup

```bash
# Set up environment
cp .env.example .env
# Edit .env with your database and Redis URLs

# Start the server (runs migrations automatically)
cargo run --release
```

The server starts at `http://localhost:3000`. Visit `/install` for the first-time setup wizard.

## CLI

```bash
# Start the HTTP server (default)
trovato serve

# Plugin management
trovato plugin list                    # List discovered plugins and status
trovato plugin install <name>          # Install plugin (run migrations, enable)
trovato plugin migrate [name]          # Run pending migrations for one or all plugins
trovato plugin enable <name>           # Enable a plugin
trovato plugin disable <name>          # Disable a plugin

# Configuration management
trovato config export [dir] [--clean]  # Export all config to YAML files
trovato config import [dir] [--dry-run] # Import config from YAML files
```

## Building Plugins

```bash
# Install WASM target
rustup target add wasm32-wasip1

# Build a plugin
cargo build -p blog --target wasm32-wasip1 --release

# Copy to plugin directory
cp target/wasm32-wasip1/release/blog.wasm plugins/blog/
```

See the [Plugin Development Guide](docs/plugin-development.md) for complete documentation.

## Running Tests

```bash
# Run all library tests
cargo test --lib

# Run all tests including integration
cargo test --all

# Run a specific plugin's tests
cargo test -p blog
```

## Configuration

### Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `DATABASE_URL` | Yes | -- | PostgreSQL connection URL |
| `PORT` | No | `3000` | HTTP server port |
| `REDIS_URL` | No | `redis://127.0.0.1:6379` | Redis connection URL |
| `DATABASE_MAX_CONNECTIONS` | No | `10` | PostgreSQL connection pool size |
| `PLUGINS_DIR` | No | `./plugins` | Path to plugin WASM files and metadata |
| `UPLOADS_DIR` | No | `./uploads` | Path for file uploads |
| `FILES_URL` | No | `/files` | Base URL for uploaded file serving |
| `TEMPLATES_DIR` | No | `./templates` | Tera templates directory |
| `CORS_ALLOWED_ORIGINS` | No | `*` | Comma-separated allowed CORS origins |
| `COOKIE_SAME_SITE` | No | `strict` | Cookie SameSite policy (`strict`, `lax`, `none`) |
| `JWT_SECRET` | No | -- | Min 32-byte secret for OAuth2 JWT signing |
| `WEBHOOK_ENCRYPTION_KEY` | No | -- | Min 32-byte key for encrypting webhook secrets |
| `RUST_LOG` | No | `info` | Tracing filter directive |

## Project Structure

```
trovato/
├── crates/
│   ├── kernel/              # HTTP server, plugin runtime, core services
│   │   └── src/
│   │       ├── services/    # OAuth, webhooks, image styles, redirects, etc.
│   │       ├── middleware/   # Auth, rate limiting, redirects, language, aliases
│   │       ├── routes/      # HTTP route handlers
│   │       ├── content/     # Content type, item, and block editor management
│   │       ├── cron/        # Scheduled tasks
│   │       ├── gather/      # Query engine, builder, and admin service
│   │       ├── stage/       # Content staging and hierarchy
│   │       ├── file/        # File management
│   │       ├── plugin/      # WASM plugin runtime
│   │       ├── tap/         # Extension point registry and dispatch
│   │       └── theme/       # Tera template engine
│   ├── plugin-sdk/          # SDK types and host function bindings
│   ├── plugin-sdk-macros/   # #[plugin_tap] proc macros
│   ├── test-utils/          # Test fixtures and mock builders
│   └── wit/                 # WIT interface definitions
├── plugins/                 # 17 standard WASM plugins
│   ├── blog/
│   ├── media/
│   ├── oauth2/
│   ├── webhooks/
│   ├── image_styles/
│   ├── ...                  # (see Standard Plugins section)
│   └── config_translation/
├── templates/               # Tera templates for HTML rendering
├── static/                  # Static assets (CSS, JS)
├── migrations/              # Core database migrations
└── docs/                    # Documentation
```

## Documentation

| Document | Description |
|----------|-------------|
| [Plugin Development Guide](docs/plugin-development.md) | Complete guide for plugin authors |
| [Plugin Quick Reference](docs/plugin-quick-reference.md) | Condensed API reference |
| [API Reference](docs/api-reference.md) | HTTP API endpoints |
| [Building Your First Site](docs/building-your-first-site.md) | Getting started tutorial |
| [Architecture](docs/design/Architecture.md) | System architecture overview |
| [Content Model](docs/design/Design-Content-Model.md) | Content types, fields, and items |
| [Query Engine](docs/design/Design-Query-Engine.md) | Gather query building and execution |
| [Plugin System](docs/design/Design-Plugin-System.md) | Plugin loading, taps, and sandboxing |
| [Plugin SDK](docs/design/Design-Plugin-SDK.md) | SDK design and host function API |
| [Block Editor Guide](docs/block-editor/user-guide.md) | Editor.js block editor usage |
| [Block Editor Config](docs/block-editor/configuration.md) | Block editor configuration reference |
| [Custom Block Types](docs/block-editor/custom-block-types.md) | Creating custom block type plugins |
| [Render & Theme](docs/design/Design-Render-Theme.md) | Rendering pipeline and theming |
| [Web Layer](docs/design/Design-Web-Layer.md) | HTTP layer, routing, and middleware |
| [Infrastructure](docs/design/Design-Infrastructure.md) | Caching, cron, sessions, metrics |

---

*This project is being developed with AI assistance.*
