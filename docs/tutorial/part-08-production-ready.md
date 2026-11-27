# Part 8: Production Ready

Part 7 took Ritrovo global with bilingual content and a REST API. Part 8 hardens everything for production: caching that scales, batch operations for bulk work, S3 storage for files, observability through metrics, and a test suite that proves it all works.

This is the capstone. By the end, Ritrovo is not a demo — it is a production-grade conference platform backed by infrastructure that can handle real traffic, real data, and real failures.

**Start state:** A fully featured bilingual conference site with 5,000+ conferences, five plugins, threaded comments, subscriptions, and a REST API.
**End state:** Two-tier caching with tag-based invalidation, batch operations for bulk content management, S3-compatible file storage, Prometheus metrics, cron with distributed locking, comprehensive test coverage, and load testing.

> **Implementation note:** All infrastructure described in this part is fully implemented. The cache layer (`cache/`), batch service (`batch/`, 349 lines), S3 storage (`file/storage.rs`, 403 lines), cron system (`cron/`, 1,379 lines), metrics endpoint (`metrics/`, 300 lines), and rate limiting (`middleware/rate_limit.rs`, 247 lines) are operational. The test suite has 727 unit tests and 325 integration tests across 15 test files. A load testing tool (`benchmarks/load-test/`, 327 lines) exists alongside the `goose` plugin for runtime performance monitoring.

---

## Step 1: Caching

Every database query Ritrovo makes costs time. With 5,000+ conferences and multiple Gather queries per page, uncached queries would make the site sluggish. Trovato caches at two levels to keep response times low.

### The Architecture

```
Request → service caches (Moka, in-process) → Database
Request → cache layer (Redis, shared)        → Database
```

**Service caches (Moka):** Items, users, permissions, content types, categories and gather queries are each cached in-process by their service, with the TTLs below. Hits are sub-microsecond. When one server writes, it clears its own copy and bumps a generation counter in Redis; every server polls the counters every 250 ms and clears a cache whose counter moved, so a change is current everywhere within about a second (see `docs/deployment/multi-server.md`).

**Cache layer (Redis):** A shared cache with a 5-minute TTL for computed values. All server instances read the same Redis, so a value filled or invalidated by one instance is the same for all of them. It has no in-process tier.

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
# Cache hits and misses are counted per cache in Prometheus metrics
curl -s http://localhost:3000/metrics | grep -E '^cache_(hits|misses)_total'
```

Every kernel cache has its own pair of series, named by the `cache` label. Load
an item page twice and the item cache shows the miss, then the hit:

```
cache_hits_total{cache="item"} 1
cache_misses_total{cache="item"} 1
cache_hits_total{cache="layer_l2"} 0
cache_misses_total{cache="layer_l2"} 0
```

The names are `item`, `field_access`, `user`, `permissions`, `content_type`,
`gather_query`, `gather_distinct`, `category`, `category_tags`, `menu`, `locale`,
`template_suggestion`, and `layer_l2` for the shared cache layer described above
(named for when it was the second of two tiers).

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

- **10 MB max file size** — the `MAX_FILE_SIZE` constant; a larger upload is a `413`
- **Extension agreement** — a filename whose extension does not map to the validated type is refused (a PNG named `x.svg` is not stored)
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

By default, the kernel runs cron on its own: a built-in scheduler calls it every 300 seconds (`cron_interval_secs` in site settings, or the `CRON_SCHEDULER_INTERVAL_SECS` environment variable), so a freshly installed site publishes scheduled content, rebuilds its search index and cleans up after itself with no external caller configured and no sidecar container. The cron report (`/admin/reports/cron`) shows whether the scheduler is on and its interval, and lets an administrator change it.

An external trigger (systemd timer, Kubernetes CronJob, or manual curl) still works exactly as before, side by side with the built-in scheduler — both call the same `run`, which takes a Redis lock, so whichever lands first in a given window runs cron and the other is told another instance already is:

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
- **HTTP request count** — `http_requests_total`, by method, route and status
- **HTTP request duration** — `http_request_duration_seconds`, a histogram with the same labels
- **Cache hits and misses** — `cache_hits_total` and `cache_misses_total`, by `cache`
- **Active connections** — `http_active_connections`, a gauge (registered, not yet recorded)
- **Database query duration** — histogram

The `path` label on the HTTP series is the route template that answered, such as
`/item/{id}`, never the address asked for, so there is one series per route and
not one per item. A request no route matched is counted under `<fallback>`: the
fallback serves URL aliases (`/conferences/rustconf-2026`) and answers 404s, so
both land there.

```bash
curl -s http://localhost:3000/metrics | grep -E '^http_requests_total'
# http_requests_total{method="GET",path="/item/{id}",status="200"} 2
```

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

This creates one YAML file per config entity: item types, gather queries, categories, tags, URL aliases, languages, roles, stages, tiles, menu links, files, and content items. Each permanent file is written as `file.{uuid}.yml` with its bytes beside it under `files/`. The export is a complete snapshot that can recreate the site on a fresh database.

### Import

Import configuration from a directory:

```bash
cargo run --release --bin trovato -- config import ./my-config/
# Dry run first:
cargo run --release --bin trovato -- config import ./my-config/ --dry-run
```

Entity types are imported in dependency order: variables → languages → roles → item types → categories → tags → search field configs → gather queries → stages → URL aliases → files → items → item translations → tiles → menu links. This ensures foreign key references resolve correctly.

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
| Role | `role.{uuid}.yml` | `role.a1b2c3d4-....yml` |
| Stage | `stage.{uuid}.yml` | `stage.a1b2c3d4-....yml` |
| Item | `item.{uuid}.yml` | `item.a1b2c3d4-....yml` |
| Tile | `tile.{uuid}.yml` | `tile.a1b2c3d4-....yml` |
| Menu link | `menu_link.{uuid}.yml` | `menu_link.a1b2c3d4-....yml` |
| Search config | `search_field_config.{uuid}.yml` | `search_field_config.a1b2c3d4-....yml` |
| File | `file.{uuid}.yml` plus its bytes | `file.a1b2c3d4-....yml`, `files/programme.pdf` |

### Shipping a file

A config set can carry the files its items reference. The YAML holds the metadata
and a `path` to the bytes, relative to the config set's directory; the bytes stay in
their own file, never base64 in YAML:

```yaml
# file.0193a5a0-0005-7000-8000-000000000001.yml
id: 0193a5a0-0005-7000-8000-000000000001
filename: programme.pdf
filemime: application/pdf
alt_text: Conference programme   # optional; for images
path: files/programme.pdf        # optional; this is the default
```

```yaml
# item.0193a5a0-0006-7000-8000-000000000001.yml
id: 0193a5a0-0006-7000-8000-000000000001
type: conference
title: RustConf 2026
fields:
  field_logo: 0193a5a0-0005-7000-8000-000000000001
```

Import validates the bytes exactly as an upload is validated: at most 10 MB, a type
in the upload allowlist (SVG is not), content whose magic bytes match `filemime`, and
a filename extension that maps to it. The `path` must be relative, may not use `..`,
and may not lead outside the directory through a symlink. A file that fails any of
these fails the whole import, and nothing in the set is written. The file is stored
`Permanent` under the declared id, so an item's File field can name it; re-importing
keeps the id and storage path and rewrites the bytes only when they changed.

Files are not staged: a file saved through a stage's config storage goes straight to
live, because its bytes live in file storage rather than a revision table, and an
item on any stage that references it needs it to exist.

The filename ID is always the entity's own identifier, which is what `config
export` writes. Roles, stages, tiles and menu links are keyed by UUID, so their
filenames carry the UUID rather than the machine name; the machine name lives in
the file. `docs/tutorial/config/README.md` indexes the tutorial's set by machine
name if you need to find one by hand.

### Import is all-or-nothing on validation

Import parses and schema checks every file, and resolves every reference it
makes, before it writes anything. If any file in the set fails, the run prints
every failure with its filename and reason, exits non-zero, and writes nothing —
so a typo cannot leave a partially applied config set behind reporting success.
This matters most for roles and stages, which have no admin form: `config import`
is their only management path, so a file that failed silently was an entity that
never arrived with nothing saying why.

`--dry-run` runs exactly that validation and skips the writes, so it fails on
anything the real run would fail on.

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
