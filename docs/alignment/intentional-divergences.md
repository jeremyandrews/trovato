# Intentional Divergences from Drupal 6

Each section documents: what D6 did, what Trovato does differently, why, and the benefits gained. Cross-references the Decision Log in `docs/design/Design-Project-Meta.md` Section 21.

---

## 1. WASM Sandboxed Plugins vs PHP Modules

**D6 behavior:** Modules were PHP files loaded into the same process. Any module could read/write the filesystem, make network calls, access global variables, or crash the entire process. A single malicious or buggy module compromised the whole server.

**Trovato approach:** Plugins compile to WASM and run inside Wasmtime with WASI stubs. Filesystem access returns `ENOSYS`. No network access. No shared mutable state. Plugin panics are caught as Wasmtime traps -- the kernel continues serving. Plugins communicate with the kernel exclusively through a structured JSON protocol over the WASM boundary.

**Why:** The single biggest operational risk with D6 was untrusted contrib modules. WASM sandboxing eliminates entire classes of vulnerabilities: arbitrary file read/write, network exfiltration, remote code execution via module upload. The performance cost (~5 us instantiation via pooled stores) is negligible.

**Benefits:** Runtime isolation, language flexibility (any WASM-targeting language), safe third-party plugins, no process crashes from plugin bugs.

**Decision Log ref:** "WASM for plugins" -- runtime loading, sandbox, language flexibility. Reversal cost: High.

---

## 2. JSONB Field Storage vs EAV Tables

**D6 behavior:** CCK (Content Construction Kit) stored each field in a separate database table (`content_type_{name}`, `content_field_{name}`). Loading an item with 10 fields required 10+ JOINs. This was the primary performance bottleneck in D6 sites with complex content types.

**Trovato approach:** All fields for an item are stored in a single `fields` JSONB column on the `item` table. Loading an item is a single-row fetch. Field queries use PostgreSQL JSONB operators (`fields->>'path'`) with expression indexes for performance.

**Why:** The N+1 JOIN problem was the most common D6 performance complaint. JSONB eliminates it entirely. PostgreSQL's JSONB indexing (GIN, expression indexes) provides query performance comparable to dedicated columns for most access patterns.

**Benefits:** Single-query item loading, flexible schema (add fields without ALTER TABLE), no migration cost for field changes, simpler codebase.

**Decision Log ref:** "JSONB for field storage" -- eliminates N+1 JOINs, flexible schema. Reversal cost: High (data migration). Benchmarking planned in Section 16.

---

## 3. RenderElement JSON vs Raw HTML

**D6 behavior:** Theme functions and templates produced raw HTML strings. Modules could inject arbitrary HTML, including scripts. XSS prevention depended on every developer calling `check_plain()` or `filter_xss()` correctly. One missed call = vulnerability.

**Trovato approach:** Plugins never produce HTML. They build `RenderElement` JSON trees using the SDK's fluent builder API. The kernel converts these to HTML via Tera templates. Text values pass through `FilterPipeline` based on the declared `#format`. Raw HTML from plugins is structurally impossible.

**Why:** XSS was the most common D6 security vulnerability class. Making it structurally impossible (rather than relying on developer discipline) eliminates the entire category.

**Benefits:** XSS prevention by construction, alterability (plugins can modify render trees before output), testability (JSON trees are easy to assert against), consistent output.

**Decision Log ref:** "Render Elements (JSON, not HTML)" -- prevents XSS and enables alterability. Reversal cost: High.

---

## 4. UUIDv7 vs Auto-Increment IDs

**D6 behavior:** All entities used sequential auto-increment integers (`node.nid`, `users.uid`). This enabled enumeration attacks (`/node/1`, `/node/2`, ...), caused merge conflicts when syncing between environments, and leaked information about system usage patterns.

**Trovato approach:** All entity IDs are UUIDv7 (time-sortable, random). No sequential integers anywhere. B-tree index locality is preserved because UUIDv7 is monotonically increasing within a time window.

**Why:** Sequential IDs are an information leak and a security risk. UUIDv7 provides the same time-sortability benefits of auto-increment (for index locality and natural ordering) without the enumeration vulnerability. Stage merging is conflict-free since IDs never collide.

**Benefits:** No enumeration attacks, safe cross-environment merging, no ID conflicts in stage publish, self-documenting creation time.

**Decision Log ref:** "UUIDv7 for all entity IDs" -- eliminates enumeration, time-sortable, merge-safe. Reversal cost: High.

---

## 5. Stages vs Simple Published Flag

**D6 behavior:** Content had a binary `status` field: published (1) or unpublished (0). There was no native content staging, preview environments, or editorial workflows. Contrib modules like Workbench Moderation added states, but they were bolted on.

**Trovato approach:** Every item carries a `stage_id` column. The `live` stage is production. Other stages (draft, campaign, etc.) are first-class entities with their own item revisions via `stage_association`. Publishing a stage atomically moves its content to live, with conflict detection. `stage_deletion` tracks staged deletes.

**Why:** Content staging is a fundamental editorial need that D6 never addressed natively. Building it into the data model from day one means every query, cache key, and access check is stage-aware -- it's not an afterthought.

**Benefits:** True content staging, preview environments, campaign workflows, atomic publish, conflict detection.

**Decision Log ref:** "Stage Schema" -- draft is a stage, not a boolean flag. Reversal cost: High. Also: "Stage as staging replacement" -- single instance serves both staging and production.

---

## 6. `is_admin` Boolean vs User 1 Magic

**D6 behavior:** User ID 1 was hardcoded as the superadmin. This "magic number" survived database migrations, was invisible in the UI, and was the source of countless bugs and confusion. If user 1 was deleted or corrupted, recovery was painful.

**Trovato approach:** Any user can be marked as admin via the `is_admin` boolean column. There is no magic user ID. Admin status is explicit, visible, and survives any ID scheme (including UUIDv7).

**Why:** Magic numbers are a maintenance liability. An explicit boolean is self-documenting, queryable, and doesn't depend on accident of ID assignment.

**Benefits:** Multiple admins without special casing, no magic numbers, survives UUID migration, visible in admin UI.

**Decision Log ref:** "`is_admin` boolean replaces User 1 magic" -- explicit, self-documenting. Reversal cost: Low.

---

## 7. SeaQuery AST vs String SQL

**D6 behavior:** Views and other query builders assembled SQL via string concatenation. While D6's database abstraction layer provided some protection, complex queries often involved manual string building, risking SQL injection.

**Trovato approach:** Gather uses SeaQuery to build queries as an AST (Abstract Syntax Tree). The AST is converted to SQL only at execution time via `PostgresQueryBuilder`. There is no string concatenation in query building. The WIT interface for plugins uses structured DB operations (`db_select`, `db_insert`), never raw SQL (except behind the `raw_sql` permission).

**Why:** SQL injection is the second most common web vulnerability. AST-based query building makes it structurally impossible in the query engine. Plugins can't inject SQL because the WASM boundary only accepts structured operations.

**Benefits:** SQL injection prevention by construction, database-portable query representation, composable query modifications via AST manipulation.

**Decision Log ref:** "SeaQuery for Gather" -- AST-based SQL, no string concatenation. Reversal cost: Medium. Also: "Structured DB API in WIT" -- prevents SQL injection from untrusted plugins. Reversal cost: High.

---

## 8. Argon2id vs MD5/SHA Password Hashing

**D6 behavior:** Drupal 6 used MD5 for password hashing (later versions moved to SHA-512 with stretching). MD5 is cryptographically broken and vulnerable to rainbow table attacks.

**Trovato approach:** Argon2id with memory-hard parameters. Argon2id is the current OWASP-recommended algorithm, resistant to GPU-based brute forcing and side-channel attacks.

**Why:** MD5 passwords can be cracked in seconds on modern hardware. Argon2id's memory-hard design makes brute forcing economically infeasible.

**Benefits:** State-of-the-art password security, resistance to GPU/ASIC attacks, configurable work factors. Migration path: legacy D6 passwords imported with `{"needs_rehash": true, "legacy_hash": "..."}` and re-hashed on first successful login.

**Decision Log ref:** "Argon2id for passwords" -- current best practice, memory-hard. Reversal cost: Low (add new algorithm, keep fallback).

---

## 9. Redis Sessions vs Database Sessions

**D6 behavior:** Sessions stored in the `sessions` database table. Session cleanup required periodic cron runs. Under high traffic, session reads added database load.

**Trovato approach:** Sessions stored in Redis via `fred` (Redis client) and `tower-sessions-redis-store` with native TTL expiration. No cleanup cron needed for session expiration -- Redis handles it automatically. Multi-server deployments share sessions via the same Redis instance.

**Why:** Redis is purpose-built for session storage: fast reads, automatic expiration, horizontal scaling. Database sessions add unnecessary load to the primary datastore.

**Benefits:** Automatic TTL-based expiration, reduced database load, multi-server session sharing, sub-millisecond session reads.

**Decision Log ref:** "Redis for sessions" -- TTL expiration native, multi-server ready. Reversal cost: Low (swap to Postgres-backed sessions).

---

## 10. Handle-Based WASM Data Access vs Full Serialization

**D6 behavior:** N/A (D6 had no WASM boundary). PHP modules accessed data directly via global state and function calls.

**Trovato approach:** Two data access modes across the WASM boundary:
- **Handle-based (default):** Plugin receives an opaque `i32` handle. Reading/writing fields calls host functions (`item_get_field`, `item_set_field`) that cross the WASM boundary one field at a time. Minimal serialization overhead per call.
- **Full serialization (opt-in):** Entire data structures serialized as JSON across the boundary. Used for complex mutations like form alter where plugins need to restructure the entire object.

**Why:** Serializing large JSON objects across the WASM boundary for every tap invocation would add 10-50ms per call on complex items. Handle-based access avoids this for the common case (reading a few fields to build a render element). Full serialization remains available for cases that genuinely need the whole object.

**Benefits:** ~5x performance improvement for typical item view taps, reduced memory allocation, SDK abstracts the mode choice from plugin authors.

**Decision Log ref:** "Handle-based data access (default)" -- avoids serialization bottleneck. Reversal cost: High. Also: "Pooled Stores" -- solves `!Send` concurrency issues. Reversal cost: High.

---

## 11. Two-Tier Cache vs Database Cache

**D6 behavior:** Cache stored in database tables (`cache`, `cache_menu`, `cache_filter`, etc.). Every cache read was a database query. Named cache bins provided logical separation.

**Trovato approach:** L1 = Moka in-process cache (10,000 entries, 60s TTL). L2 = Redis (300s TTL). Tag-based invalidation instead of named bins. Lua scripts in Redis provide atomic tag-based clearing.

**Why:** Database-backed cache defeats the purpose when the database is the bottleneck. In-process L1 avoids network round-trips for hot data. Redis L2 provides shared cache across multiple server instances.

**Benefits:** Sub-microsecond L1 hits, shared L2 across instances, tag-based invalidation for precise cache clearing, no database load from cache reads.

**Decision Log ref:** "Cache tags" -- structured invalidation prevents both stale data and over-clearing. Reversal cost: Medium.

---

## 12. PostgreSQL-Only vs Multi-Database Support

**D6 behavior:** Drupal 6 supported MySQL, PostgreSQL, and (via contrib) SQLite through its database abstraction layer. The vast majority of D6 sites ran on MySQL. Database-portable SQL was a core requirement.

**Trovato approach:** PostgreSQL-only. The codebase uses PostgreSQL-specific features extensively: JSONB columns and operators for field storage, `gen_random_uuid()` for ID generation, recursive CTEs for category hierarchy, `tsvector`/`tsquery` for full-text search, GIN indexes, `FOR UPDATE SKIP LOCKED` for queue processing, and expression indexes on JSONB paths.

**Why:** Supporting multiple databases means using the lowest common denominator of SQL features. PostgreSQL's JSONB is the foundation of Trovato's field storage model -- there is no MySQL equivalent with comparable query performance. Recursive CTEs, full-text search, and expression indexes are all PostgreSQL features that would require entirely different implementations on other databases. The database abstraction overhead is not worth it when the architecture depends on PostgreSQL-specific capabilities.

**Benefits:** Full use of PostgreSQL's advanced features, simpler codebase (no database abstraction layer), better performance (no lowest-common-denominator SQL).

**Trade-off:** This is the single biggest migration barrier for existing D6 sites running MySQL. Migration requires a MySQL-to-PostgreSQL data migration step in addition to the D6-to-Trovato schema migration.

**Decision Log ref:** "JSONB for field storage" and "PostgreSQL full-text search" -- both decisions that lock Trovato to PostgreSQL.

---

## 13. Additive Preprocess Taps vs Mutable Theme Variables

**D6 behavior:** `hook_preprocess_*` received a `&$variables` reference. Any module could overwrite any variable set by a previous module, leading to unpredictable clobber bugs in complex sites.

**Trovato approach:** Preprocess taps return additions, not mutations. The kernel merges returned variables into the template context. A plugin cannot remove or overwrite variables set by another plugin.

**Why:** Variable clobbering was a common source of hard-to-debug theme issues in D6. The additive model guarantees that each plugin's contributions are preserved.

**Benefits:** No clobber bugs, predictable template variables, easier debugging.

**Decision Log ref:** "Additive preprocess taps" -- plugins return additions, not mutations. Reversal cost: Low (change to mutation model if needed).
