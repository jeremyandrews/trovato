# Phase 6: Files, Search, Cron, & Hardening

## Gate Criterion
> All subsystems functional under load.

Specific verification:
1. File upload works (local + S3), orphans cleaned by cron
2. Search returns ranked results, indexes update on item save
3. Cron runs on exactly one server, queue items processed
4. Rate limiting prevents abuse, metrics endpoint exposes data
5. Load test passes: 100 concurrent users, <100ms p95 latency

---

## Current Status

| Component | Database | Code | Endpoints | Status |
|-----------|----------|------|-----------|--------|
| **Files** | ✅ file_managed | ✅ FileService + LocalFileStorage | ✅ /api/file/upload, /api/file/{id} | 100% |
| **Search** | ✅ (search_vector + GIN + config) | ✅ SearchService | ✅ /search, /api/search, admin UI | 100% |
| **Cron/Queue** | ✅ | ✅ CronService + RedisQueue | ✅ /cron/:key, /cron/status | 100% |
| **Rate Limiting** | ✅ Redis | ✅ RateLimiter middleware | ✅ (integrated) | 100% |
| **Metrics** | N/A | ✅ Metrics struct | ✅ /metrics | 100% |
| **Cache Layer** | N/A | ✅ CacheLayer (Moka L1 + Redis L2) | N/A | 100% |
| **Batch API** | ✅ Redis | ✅ BatchService | ✅ /api/batch/* | 100% |

### Phase 6A Complete (2026-02-13)
- ✅ `CacheLayer` with Moka L1 + Redis L2, tag-based invalidation, stage-scoped keys
- ✅ `search_field_config` table for per-bundle search field weights
- ✅ `item_search_update()` trigger for automatic search indexing
- ✅ `SearchService` with full-text search, ranking, and snippets
- ✅ `/search` HTML endpoint and `/api/search` JSON endpoint
- ✅ `CronService` with distributed Redis locking (SET NX EX pattern)
- ✅ `RedisQueue` for background task processing
- ✅ `/cron/:key` endpoint (secret key protected)
- ✅ `/cron/status` endpoint (admin only)
- ✅ Heartbeat pattern to extend lock TTL for long-running tasks

### Phase 6B Complete (2026-02-13)
- ✅ `file_managed` table with status tracking (temporary/permanent)
- ✅ `FileStorage` trait with `LocalFileStorage` implementation
- ✅ `FileService` for file CRUD operations
- ✅ `/api/file/upload` endpoint with multipart form support
- ✅ `/api/file/{id}` for file info retrieval
- ✅ Temporary file cleanup in cron

### Phase 6C Complete (2026-02-13)
- ✅ `RateLimiter` middleware with Redis-backed distributed counting
- ✅ `Metrics` struct with Prometheus counters and histograms
- ✅ `/metrics` endpoint in Prometheus format
- ✅ `http_requests_total`, `cache_hits_total`, `cache_misses_total` metrics

### Phase 6D Complete (2026-02-13)
- ✅ Search field configuration UI at `/admin/structure/types/{type}/search`
- ✅ Drag-and-drop file uploads with progress bar (`static/js/file-upload.js`)
- ✅ `BatchService` for long-running operations with Redis storage
- ✅ Batch API: `POST /api/batch`, `GET /api/batch/{id}`, cancel, delete
- ✅ Documentation at `docs/phase-6d-features.md`
- ✅ 63 integration tests passing

### What's Remaining
- Load testing infrastructure (goose)
- S3 storage backend (LocalFileStorage sufficient for MVP)

---

## Implementation Plan

### Epic 11: File & Media Management

#### Task 11.1: Database Migration - file_managed
**File:** `crates/kernel/migrations/20260213000001_create_file_managed.sql`

```sql
CREATE TABLE file_managed (
    id UUID PRIMARY KEY,
    owner_id UUID NOT NULL REFERENCES users(id),
    filename VARCHAR(255) NOT NULL,
    uri VARCHAR(512) NOT NULL UNIQUE,
    filemime VARCHAR(255) NOT NULL,
    filesize BIGINT NOT NULL,
    status SMALLINT NOT NULL DEFAULT 0,  -- 0=temporary, 1=permanent
    created BIGINT NOT NULL,
    changed BIGINT NOT NULL
);

CREATE INDEX idx_file_owner ON file_managed(owner_id);
CREATE INDEX idx_file_status ON file_managed(status);
CREATE INDEX idx_file_created ON file_managed(created);
```

#### Task 11.2: FileStorage Trait & Implementations
**File:** `crates/kernel/src/file/mod.rs`

```rust
pub mod storage;
pub mod service;

pub use storage::{FileStorage, LocalFileStorage, S3FileStorage};
pub use service::FileService;
```

**File:** `crates/kernel/src/file/storage.rs`

```rust
#[async_trait]
pub trait FileStorage: Send + Sync {
    async fn write(&self, uri: &str, data: &[u8]) -> Result<()>;
    async fn read(&self, uri: &str) -> Result<Vec<u8>>;
    async fn delete(&self, uri: &str) -> Result<()>;
    async fn exists(&self, uri: &str) -> Result<bool>;
    fn public_url(&self, uri: &str) -> String;
}

pub struct LocalFileStorage { base_path: PathBuf }
pub struct S3FileStorage { bucket: String, client: aws_sdk_s3::Client }
```

#### Task 11.3: File Upload Endpoint
**File:** `crates/kernel/src/routes/file.rs`

- `POST /file/upload` - multipart form data
- Validate: size limits (10MB default), MIME whitelist
- Store with status=0 (temporary)
- Return file ID and URL

#### Task 11.4: File Reference Tracking
**File:** `crates/kernel/src/file/service.rs`

- On item save: mark referenced files as permanent (status=1)
- Parse JSONB fields for file references
- Update `file_managed.status`

#### Task 11.5: Temporary File Cleanup (in Cron)
- Delete `file_managed` where `status=0` AND `created < now() - 6 hours`
- Delete actual files from storage
- Log cleanup counts

---

### Epic 12: Content Search

#### Task 12.1: Search Field Configuration Table
**File:** `crates/kernel/migrations/20260213000002_create_search_config.sql`

```sql
CREATE TABLE search_field_config (
    id UUID PRIMARY KEY,
    bundle VARCHAR(32) NOT NULL REFERENCES item_type(type),
    field_name VARCHAR(32) NOT NULL,
    weight CHAR(1) NOT NULL DEFAULT 'C',  -- A, B, C, or D
    UNIQUE (bundle, field_name)
);

-- Seed: title always weight A (handled in trigger)
```

#### Task 12.2: Search Index Trigger
**File:** `crates/kernel/migrations/20260213000003_create_search_trigger.sql`

```sql
CREATE OR REPLACE FUNCTION item_search_update() RETURNS trigger AS $$
DECLARE
    config RECORD;
    vector tsvector := ''::tsvector;
    field_value TEXT;
BEGIN
    -- Always index title as weight A
    vector := setweight(to_tsvector('english', COALESCE(NEW.title, '')), 'A');

    -- Index configured fields
    FOR config IN
        SELECT field_name, weight FROM search_field_config
        WHERE bundle = NEW.type
    LOOP
        field_value := NEW.fields->config.field_name->>'value';
        IF field_value IS NOT NULL THEN
            vector := vector || setweight(
                to_tsvector('english', field_value),
                config.weight::char
            );
        END IF;
    END LOOP;

    NEW.search_vector := vector;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_item_search
    BEFORE INSERT OR UPDATE ON item
    FOR EACH ROW EXECUTE FUNCTION item_search_update();
```

#### Task 12.3: Search Service
**File:** `crates/kernel/src/search/mod.rs`

```rust
pub struct SearchService { pool: PgPool }

impl SearchService {
    pub async fn search(
        &self,
        query: &str,
        user_id: Option<Uuid>,  // for draft search
        limit: i64,
        offset: i64,
    ) -> Result<SearchResults>;

    pub async fn configure_field(
        &self,
        bundle: &str,
        field_name: &str,
        weight: char,
    ) -> Result<()>;
}
```

#### Task 12.4: Search Endpoint
**File:** `crates/kernel/src/routes/search.rs`

- `GET /search?q={query}&page={n}` - HTML results page
- `GET /api/search?q={query}` - JSON API
- Include user's drafts if authenticated
- Return ranked results with snippets

---

### Epic 13: Scheduled Operations & Background Tasks

#### Task 13.1: Cron Endpoint with Distributed Lock
**File:** `crates/kernel/src/cron/mod.rs`

```rust
pub struct CronService {
    redis: RedisClient,
    pool: PgPool,
}

impl CronService {
    pub async fn run(&self) -> Result<CronResult> {
        // Acquire distributed lock
        let lock = self.acquire_lock("cron:lock").await?;
        if lock.is_none() {
            return Ok(CronResult::Skipped);
        }

        // Start heartbeat task
        let heartbeat = self.start_heartbeat();

        // Run cron tasks
        self.cleanup_temp_files().await?;
        self.cleanup_expired_sessions().await?;
        self.cleanup_form_state_cache().await?;
        self.process_queues().await?;

        // Release lock
        self.release_lock(lock).await?;
        Ok(CronResult::Completed)
    }
}
```

#### Task 13.2: Redis Queue Implementation
**File:** `crates/kernel/src/queue/mod.rs`

```rust
#[async_trait]
pub trait Queue: Send + Sync {
    async fn push(&self, queue: &str, item: &str) -> Result<()>;
    async fn pop(&self, queue: &str) -> Result<Option<String>>;
    async fn len(&self, queue: &str) -> Result<u64>;
}

pub struct RedisQueue { client: RedisClient }
```

#### Task 13.3: Cron Route
**File:** `crates/kernel/src/routes/cron.rs`

- `POST /cron/{key}` - trigger cron with secret key
- `GET /cron/status` - show last run time (admin only)
- CLI: `cargo run -- cron` for manual trigger

#### Task 13.4: Heartbeat Pattern
- Background task extends lock TTL every 60s
- Lock expires after 5 minutes if server crashes
- Prevents premature expiration for long-running tasks

---

### Epic 14: Production Readiness

#### Task 14.1: Rate Limiting Middleware
**File:** `crates/kernel/src/middleware/rate_limit.rs`

```rust
pub struct RateLimitLayer {
    redis: RedisClient,
    config: RateLimitConfig,
}

pub struct RateLimitConfig {
    pub login: (u32, Duration),      // 5 per minute
    pub forms: (u32, Duration),      // 30 per minute
    pub api: (u32, Duration),        // 100 per minute
    pub search: (u32, Duration),     // 20 per minute
}
```

Uses Redis INCR with TTL for distributed counting.

#### Task 14.2: Prometheus Metrics
**Dependencies:** Add `prometheus-client = "0.22"` to Cargo.toml

**File:** `crates/kernel/src/metrics/mod.rs`

```rust
pub struct Metrics {
    pub http_requests: Family<HttpLabels, Counter>,
    pub http_duration: Family<HttpLabels, Histogram>,
    pub wasm_tap_duration: Family<TapLabels, Histogram>,
    pub db_query_duration: Histogram,
    pub cache_hits: Counter,
    pub cache_misses: Counter,
}
```

#### Task 14.3: Metrics Endpoint
**File:** `crates/kernel/src/routes/metrics.rs`

- `GET /metrics` - Prometheus format
- Restricted to internal IPs or auth token
- Exposes all registered metrics

#### Task 14.4: Metrics Middleware
**File:** `crates/kernel/src/middleware/metrics.rs`

Wraps requests to record:
- Request count by path/method/status
- Request duration histogram
- In-flight request gauge

---

### Epic 15: Cache Layer (Bonus - Improves Performance)

#### Task 15.1: CacheLayer Implementation
**File:** `crates/kernel/src/cache/mod.rs`

```rust
pub struct CacheLayer {
    local: moka::future::Cache<String, String>,
    redis: RedisClient,
}

impl CacheLayer {
    pub async fn get(&self, key: &str) -> Option<String>;
    pub async fn set(&self, key: &str, value: &str, ttl: Duration, tags: &[&str]);
    pub async fn invalidate(&self, key: &str);
    pub async fn invalidate_tag(&self, tag: &str);
}
```

#### Task 15.2: Wire Cache to Host Functions
Update `crates/kernel/src/host/cache.rs` to use CacheLayer.

#### Task 15.3: Stage-Scoped Cache Keys
```rust
pub fn stage_key(key: &str, stage_id: Option<&str>) -> String {
    match stage_id {
        None | Some("live") => key.to_string(),
        Some(st) => format!("st:{st}:{key}"),
    }
}
```

---

## Database Migrations Summary

| Migration | Table/Function |
|-----------|----------------|
| 20260213000001 | `file_managed` |
| 20260213000002 | `search_field_config` |
| 20260213000003 | `item_search_update()` trigger |

---

## New Files

```
crates/kernel/src/
├── file/
│   ├── mod.rs
│   ├── storage.rs      # FileStorage trait + impls
│   └── service.rs      # FileService
├── search/
│   ├── mod.rs
│   └── service.rs      # SearchService
├── cron/
│   ├── mod.rs
│   └── tasks.rs        # Individual cron tasks
├── queue/
│   └── mod.rs          # Queue trait + RedisQueue
├── cache/
│   └── mod.rs          # CacheLayer
├── metrics/
│   └── mod.rs          # Prometheus metrics
├── middleware/
│   ├── mod.rs
│   ├── rate_limit.rs
│   └── metrics.rs
└── routes/
    ├── file.rs         # /file/upload
    ├── search.rs       # /search
    ├── cron.rs         # /cron/{key}
    └── metrics.rs      # /metrics
```

---

## Files to Modify

| File | Changes |
|------|---------|
| `Cargo.toml` | Add `prometheus-client`, `aws-sdk-s3`, `governor` |
| `src/state.rs` | Add FileService, SearchService, CronService, CacheLayer, Metrics |
| `src/main.rs` | Wire new routes, middleware |
| `src/host/cache.rs` | Implement actual caching |
| `src/routes/mod.rs` | Export new route modules |

---

## Implementation Order

### Phase 6A: Foundation (Week 1-2)
1. Cache Layer (enables everything else)
2. Search trigger + basic search endpoint
3. Cron infrastructure with distributed lock

### Phase 6B: Files & Queues (Week 3-4)
4. File storage trait + local implementation
5. File upload endpoint
6. Queue implementation
7. File cleanup in cron

### Phase 6C: Production Hardening (Week 5-6)
8. Rate limiting middleware
9. Prometheus metrics
10. S3 storage backend
11. Load testing

### Phase 6D: Polish (Week 7-8)
12. Search field configuration UI
13. Drag-and-drop file uploads
14. Batch API for long operations
15. Documentation

---

## Verification Plan

### Unit Tests
- [ ] FileStorage trait implementations
- [ ] CacheLayer get/set/invalidate
- [ ] SearchService query building
- [ ] CronService lock acquisition
- [ ] Queue push/pop operations
- [ ] Rate limit counter logic

### Integration Tests
- [ ] File upload → storage → retrieval
- [ ] Item save → search index → search query
- [ ] Cron lock prevents double execution
- [ ] Queue items processed by worker
- [ ] Rate limit returns 429

### Load Tests (goose)
- [ ] 100 concurrent anonymous page views
- [ ] 50 concurrent logged-in users editing
- [ ] 20 concurrent search queries
- [ ] Mixed workload: 70% read, 20% search, 10% write

### Gate Verification
```bash
# 1. File upload works
curl -X POST -F "file=@test.jpg" http://localhost:3000/file/upload
# Verify file appears in storage

# 2. Search returns results
curl "http://localhost:3000/search?q=test"
# Verify ranked results

# 3. Cron runs once
# Start two servers, trigger cron, verify only one executes

# 4. Rate limiting works
for i in {1..10}; do curl -X POST .../user/login; done
# Verify 429 after limit

# 5. Metrics available
curl http://localhost:3000/metrics
# Verify Prometheus format

# 6. Load test passes
cargo run -p goose-tests
# Verify <100ms p95, no errors
```

---

## Dependencies to Add

```toml
# Cargo.toml [workspace.dependencies]
prometheus-client = "0.22"
aws-sdk-s3 = "1.0"
aws-config = "1.0"
governor = "0.6"
goose = "0.17"  # dev-dependency for load tests
```

---

## Risk Assessment

| Risk | Mitigation |
|------|------------|
| S3 SDK complexity | Start with LocalFileStorage, S3 as second pass |
| Distributed lock race conditions | Use proven Redis SET NX EX pattern |
| Search performance at scale | GIN index already exists, add query timeouts |
| Rate limit bypass | Use Redis for distributed counting |
| Metrics cardinality explosion | Limit label values, use buckets |

---

## Out of Scope (Deferred)

- Meilisearch/Typesense integration (Postgres FTS sufficient for MVP)
- ~~Drag-and-drop file uploads~~ ✅ Implemented in Phase 6D
- ~~Batch API progress polling~~ ✅ Implemented in Phase 6D
- Search highlighting (can add later)
- Stage-aware search (requires per-stage tsvector)
- S3 storage backend (LocalFileStorage sufficient for MVP)
- Load testing with goose (infrastructure ready, tests not written)
