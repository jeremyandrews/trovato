# Trovato Design: Infrastructure

*Sections 11-15 of the v2.1 Design Document*

---

## 11. File Handling

### Schema

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
```

### Storage Backends

Support pluggable storage backends from day one:

```rust
#[async_trait]
pub trait FileStorage: Send + Sync {
    async fn write(&self, uri: &str, data: &[u8]) -> Result<(), FileError>;
    async fn read(&self, uri: &str) -> Result<Vec<u8>, FileError>;
    async fn delete(&self, uri: &str) -> Result<(), FileError>;
    async fn exists(&self, uri: &str) -> Result<bool, FileError>;
    fn public_url(&self, uri: &str) -> String;
}
```

Implement `LocalFileStorage` and `S3FileStorage` initially.

### Upload Flow

1. Receive upload via multipart form data on `/file/upload`.
2. Validate: file size limits, MIME type whitelist, filename sanitization.
3. Write to temporary storage with `status=0`. Cleaned up by cron after 6 hours.
4. On item save, mark referenced files as permanent (`status=1`).
5. On item delete, check if any other item references the file. If not, delete it.

Public files served directly by NGINX. Private files route through the Kernel for access control.

---

## 12. Cron, Queues, and Batch Operations

### Cron Architecture

In a multi-server deployment, cron must run on exactly one server at a time. Use Redis distributed locking with a heartbeat pattern to ensure locks are held only as long as the job runs, but don't expire prematurely if a job is slow:

```rust
pub async fn run_cron(state: &mut AppState) -> Result<(), CronError> {
    let lock_key = "cron:lock";
    let lock_value = uuid::Uuid::new_v4().to_string();

    let acquired: bool = redis::cmd("SET")
        .arg(lock_key).arg(&lock_value)
        .arg("NX").arg("EX").arg(300)
        .query_async(&mut state.redis_conn).await?;

    if !acquired { return Ok(()); }

    // Background task extends lock TTL every 60s while job runs
    let job_running = Arc::new(AtomicBool::new(true));
    let heartbeat_running = job_running.clone();
    let mut heartbeat_conn = state.redis_conn.clone();
    tokio::spawn(async move {
        while heartbeat_running.load(Ordering::Relaxed) {
            let _: Result<(), _> = redis::cmd("EXPIRE")
                .arg("cron:lock").arg(120)
                .query_async(&mut heartbeat_conn).await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });

    state.plugin_registry.invoke_all("tap_cron", "");
    cleanup_temporary_files(state).await?;
    cleanup_expired_sessions(state).await?;

    job_running.store(false, Ordering::Relaxed);
    release_lock(&mut state.redis_conn, lock_key, &lock_value).await;
    Ok(())
}
```

Trigger cron via an external scheduler (systemd timer or Kubernetes CronJob), not an internal timer. Keeps the binary stateless.

### Queue API

```rust
#[async_trait]
pub trait Queue: Send + Sync {
    async fn push(&self, item: &str) -> Result<(), QueueError>;
    async fn pop(&self) -> Result<Option<String>, QueueError>;
    async fn len(&self) -> Result<u64, QueueError>;
}
```

Queue workers run during cron or as a separate long-running process. Plugins export `tap_queue_info` and `tap_queue_worker`.

### Batch API

For long-running admin operations: split into chunks, store progress in Redis, poll `/batch/{id}/status` for progress. This is Phase 5-6 work but the Queue API should be designed in Phase 1.

---

## 13. Search

### PostgreSQL Full-Text Search

#### Search Field Configuration

The search index is dynamically configurable per content type. A `search_field_config` table allows plugins and admins to register searchable fields with weight assignments:

```sql
CREATE TABLE search_field_config (
    id UUID PRIMARY KEY,
    bundle VARCHAR(32) NOT NULL REFERENCES item_type(type),
    field_name VARCHAR(32) NOT NULL,
    weight CHAR(1) NOT NULL DEFAULT 'C',  -- A, B, C, or D (tsvector weights)
    UNIQUE (bundle, field_name)
);
```

#### Dynamic Search Trigger

The `item_search_update()` trigger reads this configuration and indexes all configured fields. Title is always indexed as weight A:

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

Plugins can register searchable fields via the `tap_item_update_index` tap, or site admins can configure them through the search settings UI.

### Search Query

```rust
pub async fn search_items(
    db: &PgPool, query: &str, limit: i64, offset: i64,
) -> Result<Vec<SearchResult>, SearchError> {
    let results = sqlx::query_as::<_, SearchResult>(
        "SELECT id, title,
         ts_rank(search_vector, plainto_tsquery('english', $1)) AS rank
        FROM item
        WHERE search_vector @@ plainto_tsquery('english', $1)
            AND status = 1
        ORDER BY rank DESC
        LIMIT $2 OFFSET $3"
    )
    .bind(query).bind(limit).bind(offset)
    .fetch_all(db).await?;
    Ok(results)
}
```

### MVP Strategy: Live-Only Search

Search only indexes the Live stage for the MVP. The `search_vector` column on the `item` table is updated by the trigger on INSERT/UPDATE, and items with `stage_id != 'live'` are excluded from search results by default.

Supporting search within draft stages requires either a dedicated search engine (Meilisearch) per stage or a stage-aware tsvector approach (maintaining separate search vectors per stage revision). Both are significant engineering efforts deferred to post-MVP.

**Stage preview search:** As a lightweight compromise, stage previews can fall back to Live search results. Items modified in the stage will show stale search snippets until published. This is acceptable for editorial previews where the focus is on layout and content accuracy, not search ranking.

### Search Backend Trait

Plugins contribute to the search index via `tap_item_update_index`. For sites that outgrow Postgres full-text search, define a `SearchBackend` trait and swap in Meilisearch or Typesense:

```rust
#[async_trait]
pub trait SearchBackend: Send + Sync {
    async fn index_item(&self, item: &Item) -> Result<(), SearchError>;
    async fn search(&self, query: &str, limit: i64, offset: i64) -> Result<Vec<SearchResult>, SearchError>;
}
```

---

## 14. Caching Strategy

### Two-Tier Cache

```
┌─────────────────────────┐
│  L1: In-Process (moka)  │  ◄── Per-instance, short TTL
└────────────┬────────────┘
             │ miss
┌────────────▼────────────┐
│  L2: Redis              │  ◄── Shared, longer TTL
└────────────┬────────────┘
             │ miss
┌────────────▼────────────┐
│  PostgreSQL             │  ◄── Source of truth
└─────────────────────────┘
```

```rust
pub struct CacheLayer {
    pub local: Cache<String, String>,
    pub redis: redis::Client,
}

impl CacheLayer {
    pub fn new(redis_url: &str) -> Self {
        Self {
            local: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(60))
                .build(),
            redis: redis::Client::open(redis_url).unwrap(),
        }
    }

    pub async fn get(&self, key: &str) -> Option<String> {
        if let Some(val) = self.local.get(key).await {
            return Some(val);
        }
        let mut conn = self.redis.get_multiplexed_async_connection().await.ok()?;
        let val: Option<String> = redis::cmd("GET")
            .arg(key).query_async(&mut conn).await.ok()?;
        if let Some(ref v) = val {
            self.local.insert(key.to_string(), v.clone()).await;
        }
        val
    }

    pub async fn set(&self, key: &str, value: &str, ttl_seconds: u64, tags: &[&str]) {
        self.local.insert(key.to_string(), value.to_string()).await;
        if let Ok(mut conn) = self.redis.get_multiplexed_async_connection().await {
            let _: Result<(), _> = redis::cmd("SETEX")
                .arg(key).arg(ttl_seconds).arg(value)
                .query_async(&mut conn).await;

            // Register this key against each tag
            for tag in tags {
                let tag_key = format!("tag:{tag}");
                let _: Result<(), _> = redis::cmd("SADD")
                    .arg(&tag_key).arg(key)
                    .query_async(&mut conn).await;
            }
        }
    }

    pub async fn invalidate(&self, key: &str) {
        self.local.invalidate(key).await;
        if let Ok(mut conn) = self.redis.get_multiplexed_async_connection().await {
            let _: Result<(), _> = redis::cmd("DEL")
                .arg(key).query_async(&mut conn).await;
        }
    }
}
```

### Tag-Based Invalidation

A cached Gather of articles might be tagged `["item_list", "item:42", "item:43"]`. When Item 42 is saved, we invalidate all cache keys associated with the `item:42` tag.

We use a Lua script in Redis to do this atomically in one round-trip, preventing race conditions:

```rust
const INVALIDATE_TAG_SCRIPT: &str = r#"
    local keys = redis.call("SMEMBERS", KEYS[1])
    if #keys > 0 then redis.call("DEL", unpack(keys)) end
    redis.call("DEL", KEYS[1])
"#;

pub async fn invalidate_tag(cache: &CacheLayer, tag: &str) {
    let tag_key = format!("tag:{tag}");
    if let Ok(mut conn) = cache.redis.get_multiplexed_async_connection().await {
        // Also invalidate local cache for all tagged keys
        let keys: Vec<String> = redis::cmd("SMEMBERS")
            .arg(&tag_key)
            .query_async(&mut conn).await
            .unwrap_or_default();
        for key in &keys {
            cache.local.invalidate(key).await;
        }

        // Atomically remove from Redis
        let _: Result<(), _> = redis::Script::new(INVALIDATE_TAG_SCRIPT)
            .key(&tag_key)
            .invoke_async(&mut conn).await;
    }
}
```

On item save, invalidate tags: `item:{id}`, `item_list:{type}`, `item_list:all`. On categories term save, invalidate `term:{id}`, `term_list:{vocabulary_id}`.

### Stage-Scoped Caching

Cache keys include the stage context to prevent stage previews from polluting the live cache. When a user is viewing in a non-live stage, all cache keys are prefixed with the stage ID:

```rust
impl CacheLayer {
    /// Generate a stage-scoped cache key.
    /// Live stage uses bare keys (no prefix) for maximum cache hit rates.
    /// Non-live stages use prefixed keys to isolate preview data.
    pub fn stage_key(key: &str, stage_id: Option<&str>) -> String {
        match stage_id {
            None | Some("live") => key.to_string(),
            Some(st) => format!("st:{st}:{key}"),
        }
    }
}
```

When a stage is published, all `st:{stage_id}:*` cache keys are invalidated. This is a bulk operation using Redis `SCAN` + `DEL`:

```rust
pub async fn invalidate_stage_cache(cache: &CacheLayer, stage_id: &str) {
    if let Ok(mut conn) = cache.redis.get_multiplexed_async_connection().await {
        let pattern = format!("st:{stage_id}:*");
        let mut cursor = 0u64;
        loop {
            let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor).arg("MATCH").arg(&pattern).arg("COUNT").arg(100)
                .query_async(&mut conn).await.unwrap_or((0, vec![]));
            if !keys.is_empty() {
                let _: Result<(), _> = redis::cmd("DEL")
                    .arg(&keys).query_async(&mut conn).await;
            }
            cursor = next_cursor;
            if cursor == 0 { break; }
        }
    }
}
```

---

## 15. Error Handling and Observability

### Error Strategy

Use `thiserror` for library-level errors and `anyhow` only in the binary crate's main function. Every subsystem defines its own error enum:

```rust
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("Plugin '{0}' not found")]
    NotFound(String),
    #[error("Circular dependency involving '{0}'")]
    CircularDependency(String),
    #[error("Missing dependency '{0}'")]
    MissingDependency(String),
    #[error("WASM execution failed: {0}")]
    WasmExecution(#[from] wasmtime::Error),
    #[error("Serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Missing export '{0}'")]
    MissingExport(String),
    #[error("Missing WASM memory")]
    MissingMemory,
    #[error("Invalid UTF-8 in WASM output")]
    InvalidUtf8,
}
```

WASM plugin panics must be caught at the boundary and converted to errors. A plugin crash should never bring down the Kernel.

### Structured Logging

```rust
tracing::info!(
    plugin = %plugin_name,
    tap = %tap_name,
    duration_us = %elapsed.as_micros(),
    payload_bytes = %payload.len(),
    "Tap invocation complete"
);
```

### Metrics

Expose Prometheus metrics on `/metrics` (restricted to internal IPs via NGINX): HTTP request duration histogram, WASM tap invocation duration histogram, database query duration histogram, cache hit/miss counters, active WASM plugin instance count, Redis connection pool utilization.

### Health Check

`/health` returns 200 only if the Kernel can reach both Postgres and Redis. Returns 503 with a JSON body indicating which dependency is unreachable. Not optional for production.

---

