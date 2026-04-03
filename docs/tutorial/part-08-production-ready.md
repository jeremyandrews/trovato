# Part 8: Production Ready

Part 7 took Ritrovo global with bilingual content and a REST API. Part 8 hardens everything for production: caching that scales, batch operations for bulk work, S3 storage for files, observability through metrics, and a test suite that proves it all works.

This is the capstone. By the end, Ritrovo is not a demo — it is a production-grade conference platform backed by infrastructure that can handle real traffic, real data, and real failures.

**Start state:** A fully featured bilingual conference site with 5,000+ conferences, five plugins, threaded comments, subscriptions, and a REST API.
**End state:** Two-tier caching with tag-based invalidation, batch operations for bulk content management, S3-compatible file storage, Prometheus metrics, cron with distributed locking, comprehensive test coverage, and load testing.

> **Implementation note:** All infrastructure described in this part is fully implemented. The two-tier cache (`cache/mod.rs`, 330 lines), batch service (`batch/`, 349 lines), S3 storage (`file/storage.rs`, 403 lines), cron system (`cron/`, 1,379 lines), metrics endpoint (`metrics/`, 300 lines), and rate limiting (`middleware/rate_limit.rs`, 247 lines) are operational. The test suite has 727 unit tests and 325 integration tests across 15 test files. A load testing tool (`benchmarks/load-test/`, 327 lines) exists alongside the `goose` plugin for runtime performance monitoring.

---

## Step 1: Two-Tier Caching

Every database query Ritrovo makes costs time. With 5,000+ conferences and multiple Gather queries per page, uncached queries would make the site sluggish. Trovato uses a **two-tier cache** to keep response times low.

### The Architecture

```
Request → L1 (Moka, in-process) → L2 (Redis, shared) → Database
```

**L1 (Moka):** An in-process Rust cache with a 60-second TTL and 10,000-entry capacity. Hits are sub-microsecond — no network round-trip, no serialization. Each server instance has its own L1 cache.

**L2 (Redis):** A shared cache with a 5-minute TTL. All server instances share the same Redis, so a cache fill from one instance benefits all others. Slightly slower than L1 (network round-trip) but still faster than a database query.

**Tag-based invalidation:** When content changes, the kernel invalidates cache entries by tag rather than by key. Publishing a conference invalidates the `item:{id}` tag, which clears all cache entries tagged with that item — the detail page, every Gather listing that includes it, and the search results. This is implemented via Redis Lua scripts for atomicity.

### Cache Configuration

Cache TTLs are configurable per service via environment variables:

```bash
CACHE_TTL=60                    # Global default (seconds)
CACHE_TTL_CONTENT_TYPES=60      # Content type registry
CACHE_TTL_GATHER_QUERIES=60     # Gather query definitions
CACHE_TTL_PERMISSIONS=60        # Permission lookups
CACHE_TTL_USERS=300             # User data
CACHE_TTL_ITEMS=300             # Item lookups
CACHE_TTL_CATEGORIES=300        # Category/tag data
```

Items, users, and categories use longer TTLs (5 minutes) because they change less frequently than configuration.

### Stage-Scoped Keys

Cache keys include the stage context so preview content never leaks into the live cache:

- **Live stage:** bare key (e.g., `gather:upcoming_conferences:page:1`)
- **Non-live stages:** prefixed key (e.g., `st:{stage_id}:gather:upcoming_conferences:page:1`)

This means editors previewing draft content on the "Incoming" stage see their changes without polluting the cache that serves public visitors.

### Verify

```bash
# Cache stats are visible in Prometheus metrics
curl -s http://localhost:3000/metrics | grep cache
```

---

## Step 2: Batch Operations

Some operations are too large for a single HTTP request: reindexing 5,000 conferences, bulk-publishing staged content, regenerating all URL aliases. The **batch system** breaks these into trackable units of work with progress reporting.

### How It Works

A batch operation is created with a type and parameters, then processed in chunks. Each chunk updates the progress counter, and the caller can poll for status:

1. **Create:** `POST /admin/batch` with operation type and parameters
2. **Process:** The kernel processes items in configurable chunk sizes
3. **Poll:** `GET /admin/batch/{id}` returns progress percentage and status
4. **Complete:** Final status includes result summary or error details

### Batch States

| Status | Meaning |
|---|---|
| `pending` | Created, not yet started |
| `processing` | Currently running, progress updating |
| `completed` | Finished successfully |
| `failed` | Stopped due to error |

### Built-in Batch Operations

- **Search reindex:** Rebuilds the tsvector search index for all items of a type
- **Pathauto regenerate:** Regenerates URL aliases based on current pathauto patterns
- **Stage publish:** Atomically publishes all staged changes to live
- **Config import:** Imports a directory of YAML configuration files

---

## Step 3: File Storage & S3

In development, Trovato stores uploaded files on the local filesystem. In production, you want S3-compatible object storage for durability, CDN integration, and horizontal scaling.

### Storage Backends

Trovato's `FileStorage` trait abstracts the storage backend. Two implementations ship:

**LocalFileStorage:** Files stored in `./uploads/` with URIs like `local://2026/03/a1b2c3d4_photo.jpg`. The kernel serves them at `/files/2026/03/a1b2c3d4_photo.jpg`.

**S3FileStorage:** Files stored in an S3-compatible bucket with URIs like `s3://2026/03/a1b2c3d4_photo.jpg`. Configure via environment variables:

```bash
FILE_STORAGE=s3
S3_BUCKET=ritrovo-files
S3_REGION=us-east-1
# AWS credentials from environment or IAM role
```

### Security

Both backends enforce the same security rules:

- **10 MB max file size** — configurable via `MAX_FILE_SIZE`
- **MIME type allowlist** — images (JPEG, PNG, GIF, WebP), documents (PDF, Office), archives (ZIP, GZIP). SVG is excluded (XML-based format enables stored XSS).
- **Magic byte validation** — the kernel verifies file content matches the declared MIME type using `infer` crate magic byte detection. An ELF binary declared as `image/jpeg` is rejected.
- **Filename sanitization** — path traversal sequences (`../`) are stripped, filenames are limited to alphanumeric characters plus `.`, `-`, `_`.
- **Directory traversal prevention** — the file serve route rejects paths containing `..` or null bytes.

### Image Styles

The `trovato_image_styles` plugin provides on-the-fly image processing:

```
/files/styles/thumbnail/2026/03/a1b2c3d4_photo.jpg
```

Configured styles (thumbnail, medium, large) are generated on first request and cached. Styles are defined in the database and support resize, crop, and format conversion.

---

## Step 4: Cron & Queue Workers

Background work in Trovato runs through two cooperating systems: **cron** for scheduled tasks and **queues** for work items.

### Cron Architecture

Cron is triggered externally (systemd timer, Kubernetes CronJob, or manual curl):

```bash
curl -X POST http://localhost:3000/cron/default-cron-key
```

The cron key (`CRON_KEY` env var) prevents unauthorized triggering. Each cron run:

1. Acquires a **Redis distributed lock** with heartbeat (prevents double-execution across instances)
2. Dispatches `tap_cron` to all plugins (each plugin does its periodic work)
3. Processes **queue items** — drains each declared queue by calling `tap_queue_worker`
4. Runs **kernel maintenance**: temp file cleanup, expired session pruning, cache garbage collection

### Queue System

Plugins declare queues via `tap_queue_info` and push work items via `queue_push()`. The kernel processes queue items during cron runs with configurable concurrency:

```
ritrovo_importer → queue_push("ritrovo_import", payload)
                         ↓
              cron run → tap_queue_worker("ritrovo_import", payload)
                         ↓
              ritrovo_importer validates, deduplicates, inserts conference
```

The `ritrovo_importer` plugin uses this pattern to import conferences in batches — `tap_cron` discovers new data and queues work items, `tap_queue_worker` processes each item.

### Verify

```bash
# Trigger cron
curl -s -X POST http://localhost:3000/cron/default-cron-key | jq '.status'
# Expect: "completed"

# Check queue depth
docker exec trovato-redis-1 redis-cli LLEN queue:ritrovo_import
# Expect: 0 (drained)
```

---

## Step 5: Observability

### Prometheus Metrics

Trovato exposes a `/metrics` endpoint in Prometheus exposition format:

```bash
curl http://localhost:3000/metrics
```

Metrics include:
- **HTTP request duration** — histogram by method and path
- **HTTP request count** — counter by status code
- **Active connections** — gauge
- **Cache hit/miss rates** — per cache tier
- **Database query duration** — histogram

### Health Check

```bash
curl http://localhost:3000/health
# {"status":"healthy","postgres":true,"redis":true}
```

The health endpoint verifies both Postgres and Redis connectivity. Use it for load balancer health probes and container orchestration liveness checks.

### Rate Limiting

Per-endpoint rate limiting protects against abuse:

| Endpoint Category | Limit | Window |
|---|---|---|
| Login | 5 requests | 1 minute |
| API | 100 requests | 1 minute |
| Forms | 30 requests | 1 minute |
| Search | 20 requests | 1 minute |
| File uploads | 10 requests | 1 minute |
| Registration | 3 requests | 1 hour |

Rate limits use Redis sliding window counters. When exceeded, the server returns `429 Too Many Requests`.

### Security Headers

Every response includes security headers (added in the Inclusivity-First Foundation work):

```
Content-Security-Policy: default-src 'self'; script-src 'self'; ...
X-Frame-Options: DENY
X-Content-Type-Options: nosniff
Referrer-Policy: strict-origin-when-cross-origin
Permissions-Policy: camera=(), microphone=(), geolocation=()
Strict-Transport-Security: max-age=31536000; includeSubDomains  (HTTPS only)
```

---

## Step 6: Testing

Trovato's test suite is the safety net that lets you refactor with confidence.

### Unit Tests (727 tests)

Every kernel module has unit tests that run without a database:

```bash
cargo test -p trovato-kernel --lib
# test result: ok. 727 passed
```

Key areas covered:
- Block type validation and rendering (34 tests)
- Render tree sanitization and XSS prevention (12 tests)
- Form API element rendering (10 tests)
- Gather query building with parameterized SQL (24 tests)
- URL alias resolution and rewriting (12 tests)
- CSRF token generation and verification
- File upload security (magic bytes, MIME validation, filename sanitization)
- Config import/export YAML parsing

### Integration Tests (325 tests)

Integration tests run against a live Postgres + Redis instance:

```bash
cargo test --all -- --test-threads=1
```

These verify full request-response cycles, database operations, plugin loading, and multi-service interactions. Key test files:

| File | Tests | Coverage |
|---|---|---|
| `integration_test.rs` | 135 | End-to-end HTTP requests, plugin lifecycle |
| `item_test.rs` | 47 | Item CRUD, field validation, SDK types |
| `gather_test.rs` | 24 | Query building, filtering, pagination |
| `tutorial_test.rs` | 22 | Tutorial code block verification |
| `plugin_test.rs` | 21 | WASM loading, tap dispatch, host functions |
| `stage_test.rs` | 14 | Stage publish, overlay, conflict detection |
| `category_test.rs` | 13 | Category hierarchy, tag CRUD |

### Load Testing

The `benchmarks/load-test/` tool generates realistic traffic patterns:

```bash
cargo run -p trovato-loadtest --release -- \
  --host http://localhost:3000 \
  --users 50 \
  --duration 60
```

The `goose` plugin provides runtime performance instrumentation alongside the load test.

### Plugin Tests

Each plugin has its own tests that run in native (non-WASM) mode:

```bash
cargo test -p ritrovo_importer
cargo test -p ritrovo_cfp
cargo test -p ritrovo_access
```

Plugin tests use the `__inner_*` functions generated by `#[plugin_tap]` macros, exercising plugin logic without WASM overhead.

### CI Pipeline

All tests run in CI on every push to `main`:

| Job | What it checks |
|---|---|
| Format | `cargo fmt --all --check` |
| Clippy | `cargo clippy --all-targets -- -D warnings` |
| Test | Full test suite with Postgres + Redis |
| Coverage | Code coverage report via `cargo-llvm-cov` |
| Build | Release build of kernel + WASM plugins |
| Doc Check | `RUSTDOCFLAGS="-D warnings" cargo doc` |
| Security Audit | `cargo audit` for vulnerable dependencies |
| Terminology | No Drupal terms in Rust source files |

---

## Step 7: Configuration Management

### Export

Export the entire site configuration to YAML files:

```bash
cargo run --release --bin trovato -- config export ./my-config/
```

This creates one YAML file per config entity: item types, gather queries, categories, tags, URL aliases, languages, roles, stages, tiles, menu links, and content items. The export is a complete snapshot that can recreate the site on a fresh database.

### Import

Import configuration from a directory:

```bash
cargo run --release --bin trovato -- config import ./my-config/
# Dry run first:
cargo run --release --bin trovato -- config import ./my-config/ --dry-run
```

Entity types are imported in dependency order: variables → languages → roles → item types → categories → tags → search field configs → gather queries → stages → URL aliases → items → tiles → menu links. This ensures foreign key references resolve correctly.

### What's Importable

| Entity Type | File Pattern | Example |
|---|---|---|
| Item type | `item_type.{name}.yml` | `item_type.conference.yml` |
| Category | `category.{id}.yml` | `category.topics.yml` |
| Tag | `tag.{uuid}.yml` | `tag.a1b2c3d4-....yml` |
| Gather query | `gather_query.{id}.yml` | `gather_query.ritrovo.upcoming_conferences.yml` |
| URL alias | `url_alias.{uuid}.yml` | `url_alias.a1b2c3d4-....yml` |
| Language | `language.{code}.yml` | `language.it.yml` |
| Variable | `variable.{key}.yml` | `variable.pathauto_patterns.yml` |
| Role | `role.{name}.yml` | `role.editor.yml` |
| Stage | `stage.{machine_name}.yml` | `stage.incoming.yml` |
| Item | `item.{uuid}.yml` | `item.a1b2c3d4-....yml` |
| Tile | `tile.{machine_name}.yml` | `tile.search_box.yml` |
| Menu link | `menu_link.{menu}.{title}.yml` | `menu_link.main.conferences.yml` |
| Search config | `search_field_config.{uuid}.yml` | `search_field_config.a1b2c3d4-....yml` |

---

## What's Deferred

| Feature | Status | Notes |
|---|---|---|
| CDN integration | Future | S3 storage is CDN-ready; CDN URL rewriting is a plugin |
| Blue-green deployments | Future | Stage system provides content staging; deployment orchestration is external |
| Database replication | Future | Read replicas for horizontal read scaling |
| Distributed tracing (OpenTelemetry) | Future | Structured logging via `tracing` crate covers most needs |
| Response compression (gzip/brotli) | Future | Typically handled by a reverse proxy (nginx) |
| HTTP/2 | Future | Requires HTTPS termination, usually at the reverse proxy |
