# Trovato

A content management system built in Rust, reimagining Drupal 6's mental model with modern foundations.

## What It Is

Trovato takes the core ideas that made Drupal 6 powerful—nodes, fields, views, hooks—and rebuilds them with:

- **Axum + Tokio** for async HTTP
- **PostgreSQL + JSONB** for flexible field storage without join complexity
- **WebAssembly plugins** running in per-request sandboxes via Wasmtime
- **Content staging** built into the schema from day one

## Security Model

Plugins are untrusted code. They run in WASM sandboxes, return JSON render trees (not raw HTML), and access data through a structured API. The kernel sanitizes all output. This isn't optional—the WASM boundary enforces isolation whether plugin authors intend it or not.

## Scaling

No persistent state in the binary. PostgreSQL and Redis handle everything. Horizontal scaling works out of the box.

---

## Progress

### Phase 0: WASM Architecture Validation
Benchmarked WASM plugin performance on ARM and x86-64. Validated that full-serialization (passing complete JSON to plugins) outperforms handle-based field access by 1.2-1.6x. Confirmed pooling allocator scales to 2000+ concurrent requests with sub-millisecond p95 latency.

### Phase 1: Skeleton
Built the HTTP server foundation with Axum, PostgreSQL via SQLx, and Redis sessions. Implemented user authentication (Argon2id), role-based permissions, account lockout, password reset, and stage switching.

### Phase 2: Plugin Development Platform
Implemented the complete WASM plugin system. Created plugin loader with pooling allocator (~5µs instantiation), tap registry for hook dispatch, and 7 host function modules (item, db, user, cache, variables, request-context, logging). Built `#[plugin_tap]` proc macro for SDK. Reference blog plugin compiles to WASM with 4 tap exports. Added menu registry with path matching, dependency resolver with cycle detection, and structured error types.

### Phase 3: Content System (Complete)
Implemented the core content management functionality (Epic 4, Stories 4.1-4.11). Database schema for item_type, item, and item_revision tables with JSONB field storage, GIN indexes, and full-text search vectors. Item model with full CRUD operations and revision history. ContentTypeRegistry syncs definitions from plugins via tap_item_info and caches in DashMap. ItemService with tap integration (insert, update, delete, view, access) and proper access control aggregation (Deny wins, then Grant, else Neutral). HTTP routes for item CRUD at /item/* paths with API endpoints. Text format filter pipeline for XSS protection (plain_text, filtered_html, full_html). Auto-generated admin forms from field definitions supporting all field types. 170+ tests passing.

### Phase 4: Gather Query Engine & Categories (Complete)
Implemented the Gather query engine and Categories system. Categories and tags with DAG hierarchy (multiple parents per tag) using recursive CTEs for ancestor/descendant queries. Gather provides type-safe query building via SeaQuery with 16 filter operators including category-aware filters (HasTag, HasTagOrDescendants). ViewDefinition specifies queries declaratively; ViewDisplay configures rendering with pager support. GatherService executes queries with exposed filter resolution and stage awareness. REST API at /api/categories, /api/category/*, /api/tag/*, and view execution at /api/view/*/execute. Gate test verified: "Recent Articles" query with category hierarchy filter and pagination. 227+ tests passing.

---

*This project is being developed with AI assistance.*
