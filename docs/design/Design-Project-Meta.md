# Trovato Design: Project Meta

*Sections 16-23 of the v2.1 Design Document*

---

## 16. JSONB Benchmarking Plan

### Test Dataset

50,000 items across 5 content types, 10-20 fields per item (2-8KB each), 1-5 categories terms per item from a vocabulary of 500 terms.

### Queries to Benchmark

1. Load single item by id (baseline)
2. List 25 items of type X, sorted by created date
3. Filter by JSONB text field equality
4. Filter by JSONB integer field range
5. Filter by JSONB record reference containment
6. Combined: type filter + JSONB text filter + sort by JSONB integer field
7. Full-text search + type filter + pager

### Index Configurations to Test

Run each query against GIN only, GIN + expression indexes, and GIN + materialized columns.

### Decision Criteria

If GIN-only meets targets for 80%+ of queries, use JSONB with targeted expression indexes for the exceptions. If more than 30% of queries require expression indexes, reconsider the pure-JSONB approach and evaluate a hybrid model.

---

## 17. Migration Strategy from Existing Drupal 6

### User Migration

Import users with a flag in the `data` JSONB (`{"needs_rehash": true, "legacy_hash": "..."}`), verify against the legacy hash on login, re-hash with Argon2id on success, force password resets after a 90-day grace period.

### Item Migration

Read field definitions from `content_node_field` and `content_node_field_instance`, map each field type, query all content tables per item, flatten into JSONB, insert into the new item table. Write this as a standalone Rust binary connecting to both MySQL and PostgreSQL. Runs once.

### Categories Migration

Map Drupal 6's `term_item` table to JSONB field references. Vocabulary and term tables map almost directly.

### What Cannot Be Migrated Automatically

Custom plugin logic (PHP → WASM rewrite), theme templates (PHPTemplate → Tera manual conversion), Gather configurations (manual recreation), tap implementations in custom code.

---

## 18. Project Structure

```
trovato/
├── Cargo.toml
├── crates/
│   ├── kernel/
│   │   └── src/
│   │       ├── main.rs
│   │       ├── state.rs, request.rs, router.rs
│   │       ├── auth.rs, session.rs, permissions.rs
│   │       ├── item.rs, user.rs, field.rs
│   │       ├── form.rs, menu.rs
│   │       ├── taps.rs, plugins.rs
│   │       ├── gather.rs, categories.rs
│   │       ├── render.rs, theme.rs
│   │       ├── cache.rs, files.rs, search.rs
│   │       ├── cron.rs, queue.rs
│   │       ├── profiler.rs, metrics.rs, errors.rs
│   ├── plugin-sdk/       # Types shared with plugins (Item, User, Tap traits)
│   ├── test-utils/       # Integration testing helpers
│   └── wit/
│       └── kernel.wit
├── plugins/
│   ├── item/
│   ├── user/
│   ├── system/
│   ├── blog/
│   ├── page/
│   └── categories/
├── migrations/
│   ├── 001_users.sql through 012_form_state.sql
├── templates/
│   ├── base.html, page.html, item.html
│   ├── gather/, macros/, themes/
└── tools/
    └── migrate/              # Standalone D6 migration binary
```

---

## 19. Key Dependencies

```toml
[workspace.dependencies]
axum = "0.8"
tokio = { version = "1", features = ["full"] }
tower = "0.5"
tower-sessions = "0.13"
tower-sessions-redis-store = "0.14"
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "json", "chrono"] }
sea-query = { version = "0.32", features = ["backend-postgres"] }
wasmtime = "28"
argon2 = "0.5"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
moka = { version = "0.12", features = ["future"] }
redis = { version = "0.27", features = ["tokio-comp", "cluster-async"] }
dashmap = "6"
tera = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
chrono = { version = "0.4", features = ["serde"] }
thiserror = "2"
uuid = { version = "1", features = ["v7"] }
```

Pin exact versions in `Cargo.lock`. Test before upgrading, especially wasmtime.

---

## 20. Development Roadmap

> **Note:** This section reflects the original rough estimates. See [[Projects/Trovato/Phases]] for the authoritative timeline.

Revised estimate: **42-58 weeks** for a single senior Rust developer. The critical path runs through the WASM host implementation.

**Phase 0: The Critical Spike (2 weeks)** — Standalone spike. Benchmark handle-based vs. full-serialization data access (500 calls each). Benchmark `Store` pooling with high concurrency (100 parallel requests). Validate async host functions (WASM → Rust → SQLx bridge). Gate: written recommendation on data access mode with benchmark numbers.

**Phase 1: Skeleton (4 weeks)** — Axum server with Postgres and Redis. Login works. Session layer with Stage tracking. Profiling middleware (Gander). Queue API designed. Gate: user can log in, see their active stage in the session, log out.

**Phase 2: Plugin Kernel + SDK (8 weeks, extended from 6)** — SDK-first approach. Weeks 1-2: write three reference plugin source files (blog, page, categories) as spec. Weeks 3-5: build plugin-sdk crate. Weeks 6-8: build Kernel-side plugin loader, tap dispatcher, RequestState. Gate: blog plugin registers a route, reads fields via handle-based host functions, returns a JSON Render Element.

**Phase 3: Content, Fields, & Stages (8 weeks)** — Admin can create content types, attach fields. Item CRUD with JSONB fields. Stage schema implementation (`stage_association`). Text Filter Pipeline (sanitization logic). Revisions created on every save. Gate: create a content type with 5 fields, create/edit/revert an item, see different content in different stages.

**Phase 4: Gather Query Engine & Categories (8 weeks)** — SeaQuery integration. View definitions generate correct SQL. Categories vocabularies, terms, and hierarchy functional. Categories filters work in Gather. Render Tree integration for output. Gate: a View listing "Recent Articles" filtered by categories term with pager renders correctly with stage filtering.

**Phase 5: Form API, Theming, & Admin UI (8 weeks)** — Form API renders/validates/submits. `tap_form_alter` works. Template suggestions and preprocess taps functional. AJAX support for form alterations. Gate: admin can create a content type and manage permissions through the UI; forms support AJAX "Add another item."

**Phase 6: Files, Search, Cron, & Hardening (8 weeks)** — File upload/storage with S3 backend. Full-text search (Live stage only). Cron with distributed locking and heartbeats. Prometheus metrics. Load testing with goose. Gate: all subsystems functional under load.

Each phase has a gate. If a phase runs past its upper estimate, stop and reassess scope.

### What This Estimate Does Not Include

The 40-55 week range gets you a working system that proves the architecture. It does not account for: debugging WASM tooling issues (budget 2-4 extra weeks), writing the plugin SDK and at least 3 reference plugins (blog, page, categories — budget 3-4 weeks), writing comprehensive tests (budget 3-4 weeks), documentation for plugin authors (budget 1-2 weeks). A more honest range for production-ready (not production-deployed, but ready) is **50-65 weeks**.

---

## 21. Decision Log

| Decision | Rationale | Reversal Cost |
|---|---|---|
| Pooled Stores | Solves `!Send` concurrency issues while keeping isolation. | High — fundamental to the runtime model. |
| Render Elements (JSON, not HTML) | Prevents XSS and enables alterability (unlike raw HTML). | High — changes the entire theme layer. |
| Structured DB API in WIT | Prevents SQL Injection from untrusted plugins. | High — changes the WIT interface. |
| Request Context | Solves lack of static state in WASM plugins. | Medium — change affects all plugin code. |
| JSONB for field storage | Eliminates N+1 JOINs. Flexible schema. | High — data migration. Benchmark in Phase 3. |
| Stage Schema | "Draft" is a stage, not a boolean flag. Future-proofs editorial workflows. | High — core query logic depends on it. |
| WASM for plugins | Runtime loading, sandbox, language flexibility. | High — new plugin API. Phase 0 spike mitigates. |
| Redis for sessions | TTL expiration native. Multi-server ready. | Low — swap to Postgres-backed sessions. |
| SeaQuery for Gather | AST-based SQL. No string concatenation. | Medium — affects all Gather code. |
| Tera for templates | Jinja2-compatible. Compiled at load time. Rust-native. | Medium — template syntax change. |
| PostgreSQL full-text search (Live-only) | No external dependency. Good enough for most sites. Stage search requires external engines. | Low — `SearchBackend` trait allows drop-in replacement. |
| Argon2id for passwords | Current best practice. Memory-hard. | Low — add new algorithm, keep fallback. |
| Additive preprocess taps | Plugins return additions, not mutations. Prevents clobber. | Low — change to mutation model if needed. |
| Separate migration binary | Runs once. No runtime coupling to Kernel. | None — standalone by design. |
| Cache tags | Structured invalidation prevents both stale data and over-clearing. | Medium — plumbed through all cache-setting code. |
| Handle-based data access (default) | Avoids serialization bottleneck at WASM boundary. Full-serialization available as opt-in for complex mutations. | High — changes the WIT interface and SDK. Phase 0 validates. |
| SDK-first plugin design | Write the code you want devs to write, then build the host. Reduces rework. | Low — affects process, not architecture. |
| Dynamic search field config | Static trigger was hardcoded to title + body. Real sites need arbitrary searchable fields per content type. | Low — config table + trigger replacement. |
| Inter-plugin `invoke_plugin` | Edge-case escape hatch. Most communication via taps + shared DB. | Low — remove the host function if unused. |
| UUIDv7 for all entity IDs | Eliminates enumeration attacks, enables safe stage merging, follows Sitter precedent. Time-sortable (unlike v4) preserves B-tree index locality. | High — changes every table schema and struct. |
| Stage as staging replacement | Single instance serves both staging and production. Edit in stages, preview safely, publish atomically. Eliminates staging environment overhead. | High — stage_id column on items, stage_deletion table, stage-scoped cache/queries. |
| `is_admin` boolean replaces User 1 magic | Explicit admin flag instead of Drupal's "User ID 1 is god." Survives UUID migration, is self-documenting. | Low — single column change. |

### Decisions Evaluated and Rejected

For completeness, these architectural alternatives were evaluated and intentionally not adopted:

**"Cut the Render Engine, go Headless First."** The Render Tree + Tera is the differentiator. Cutting it makes this "yet another headless CMS" in a space with Strapi, Directus, and Payload. The whole point is that non-developers can build sites. Stays.

**"JSONB sorting needs a Query Analyzer."** Expression indexes and the benchmarking plan (Section 16) already address this. A query analyzer is a nice-to-have for a later version, not a prerequisite. The real mitigation is: expression indexes for frequently-sorted fields, Redis cache for expensive Gather queries.

**"Tera DX friction from strict templating."** Tera's strictness is a feature. PHPTemplate's looseness was a security and maintenance disaster. The fix is a rich set of custom Tera filters and functions, not loosening the template engine.

**"Admin UI must exist from Phase 1."** The document is a Kernel architecture spec. The auto-generated CRUD forms added to Phase 3 unblock testing. A polished admin UI is a product concern, not an architecture concern, and building it concurrently with the Kernel is how projects stall.

---

## 22. Remaining Gaps and Honest Assessments

This section catalogs what the plan still hand-waves, over-promises, or hasn't thought through deeply enough. Each gap is now assigned to a phase (or explicitly deferred) with a decision criterion.

### Serialization Cost: The "Chatty" Boundary

**Phase: 0 — RESOLVED**

Even with pooling, moving large JSON objects across the WASM boundary burns CPU. A complex admin form with 50+ elements, serialized to JSON, deserialized in WASM, modified, re-serialized, repeated for every plugin implementing `tap_form_alter` — this could easily add 10-50ms per form render.

**Resolution:** Dual-mode data access: handle-based (default) and full serialization (opt-in for complex mutations like form alter). Phase 0 benchmarks both modes. The SDK abstracts this away — plugin authors never touch raw handles. Form alter taps remain full-serialization (Mode 2) since forms are complex structures that plugins need to restructure; this is acceptable because form builds are infrequent relative to item views. See [[Projects/Trovato/Design-Plugin-System|Plugin System]] and [[Projects/Trovato/Design-Plugin-SDK|SDK Spec]].

**Decision criterion:** If handle-based is >5x faster than full serialization for item taps (expected), it becomes the default WIT pattern. If full serialization is acceptable (<1ms p95 for 4KB), keep the simpler API.

### Plugin-to-Plugin Communication: Undefined

**Phase: 4 — RESOLVED**

The WIT interface defines how the Kernel talks to plugins, but not how plugins talk to each other. In Drupal 6, plugin A could call `plugin_invoke('pluginB', 'some_function', $args)`.

**Resolution:** `invoke_plugin` host function in the WIT interface, routed through the `RequestState` using the same per-request store pool. The SDK spec (§6) includes the `plugin-api` interface with `invoke` and `plugin-exists` functions. See [[Projects/Trovato/Design-Plugin-System|Plugin System §5]]. Implemented in Phase 4. Most inter-plugin communication happens through taps and shared DB state already; `invoke_plugin` handles the edge cases.

### Access Control on Items: Only Permission-Based

**Phase: 3 — OPEN, needs design work**

The permission system checks `user_has_permission(state, user, "access content")` but there's no item-level access control. Drupal 6 had `tap_access` (later `tap_item_access`) that let plugins grant or deny access per item. Without this, you can't build "users can only edit their own content" or "unpublished content is only visible to admins."

**What's needed:** Design the access check flow and grant/deny aggregation model during Phase 3 (content CRUD). Specifically:

1. Add `tap_item_access` to the WIT interface exports (the SDK spec §3 already defines `AccessResult` as `{Neutral, Allow, Deny}`)
2. Define the aggregation rule: **Drupal's model** — any `Allow` grants access unless there's an explicit `Deny`. `Neutral` defers to the next plugin. If all return `Neutral`, fall back to the role-based permission check.
3. Wire `tap_item_access` into `item_load` and `item_view` handlers so every item load respects per-item grants.
4. The blog and page reference plugins should demonstrate basic "edit own content" access logic.

**Decision criterion:** After Phase 3, a non-admin user can create content and edit only their own items. Unpublished items are invisible to users without the `view unpublished content` permission.

### Gather Exposed Filters: Not Designed

**Phase: 4 (basic) + 5 (full) — OPEN, spans two phases**

The Gather definition includes an `exposed: bool` flag on filters but doesn't describe how exposed filters are rendered, submitted, or applied. Exposed filters are forms — they need the Form API. They need to be rendered as part of the View output. They need to read from query parameters.

**What's needed:** Split into two deliverables:

*Phase 4 (Gather engine):* Basic exposed filters that read from query parameters and apply to the SeaQuery builder. No form rendering — just `?field_tags=5&status=1` parsed from the URL and injected into the WHERE clause. This unblocks API-style filtered listings.

*Phase 5 (Form API):* Full exposed filter forms rendered as part of the View output using the Form API. The form submits as GET (query parameters), which feeds back into the Phase 4 mechanism. Add `tap_gather_exposed_form_alter` so plugins can modify exposed filter forms.

**Decision criterion (Phase 4):** A Gather listing of articles accepts `?field_tags=5` as a URL parameter and returns only matching items. **Decision criterion (Phase 5):** The same listing renders a filter form above results; submitting the form updates results.

### Stage Merge Conflicts: Not Solved

**Phase: Post-MVP — accept the risk**

The current design assumes a "Last Publish Wins" model. It does not handle merge conflicts (e.g., User A edits Item 1 in "Spring Campaign" while User B edits Item 1 in "Live"). Building a UI to resolve JSONB field conflicts is a significant undertaking.

**Accepted risk:** For v1, "Last Publish Wins" is the model. The schema already supports conflict *detection* — on publish, compare the stage association's `target_revision_id` against the item table's current `current_revision_id`. If they diverge, warn the publisher. But the actual merge/diff UI is product work that requires field-level diff rendering, which depends on field type plugins understanding their own diff semantics. Defer to post-MVP.

**Mitigation:** Add a `conflict_check` flag to `stage_publish()` that returns a warning (not an error) if the Live revision has changed since the stage override was created. Let the publisher decide whether to overwrite. This is a few hours of work in Phase 3; add it there.

### The Plugin SDK: Needs Substance

**Phase: 2 — RESOLVED**

**Resolution:** The [[Projects/Trovato/Design-Plugin-SDK|Plugin SDK Spec]] document now provides comprehensive coverage: plugin structure, proc macros, core types (ItemHandle, TapContext, RenderElement, AccessResult), the full reconciled WIT interface, mutation model, error handling, plugin lifecycle, testing patterns, and a complete blog plugin example.

### No Testing Strategy

**Phase: 1 (infrastructure) + ongoing per phase — OPEN, needs integration into each phase**

No testing strategy was specified in the original design. For a system this complex, you need unit tests, integration tests, and end-to-end tests. The WASM boundary especially needs integration tests because serialization bugs will be the most common failure mode.

**What's needed:** Build the test infrastructure in Phase 1, then budget test time within each phase (not as a separate block at the end):

*Phase 1:* Set up `test-utils` crate with: test database provisioning (create/drop per test), Redis test instance, fixture loading helpers, assertion macros for HTTP responses. Establish the pattern: every PR includes tests for the code it adds.

*Phase 2:* WASM boundary integration tests — compile a test plugin, load it, invoke taps, verify host function calls return correct data. The `MockKernel` and `TestEnvironment` from SDK spec §9 are the starting point.

*Phase 3:* Item CRUD integration tests (create, load, update, delete, revert), field validation unit tests, stage-aware loading tests.

*Phase 4:* Gather query builder unit tests (verify generated SQL), categories hierarchy tests (recursive CTE correctness).

*Phase 5:* Form submission end-to-end tests (render → fill → submit → validate → save), CSRF token verification tests.

*Phase 6:* Load tests with goose (already mentioned), file upload/download tests, cron locking tests.

**Decision criterion:** No phase gate passes without test coverage for that phase's deliverables. Minimum: one integration test per major flow, unit tests for all validation/query-building logic.

### Rate Limiting and Abuse Prevention: Not Mentioned

**Phase: 6 — RESOLVED**

No rate limiting on login attempts (brute force), no rate limiting on form submissions (CSRF token doesn't prevent automated abuse), no rate limiting on API endpoints.

**Resolution:** Rate limiting via Tower middleware in Phase 6. Implement as a `tower::Layer` with configurable per-route limits stored in Postgres config or TOML. Minimum requirements: login attempts (5 per minute per IP), form submissions (30 per minute per session), API endpoints (configurable per route). Use Redis for distributed rate limit counters in multi-server deployments.

### Component Model Maturity

**Phase: Post-MVP (Year 2) — accept the risk**

The WASM Component Model is evolving. We are pinning to `wasmtime` and WASI Preview 1 for now, but migration to newer WASI versions will be required later.

**Accepted risk:** This is an industry-wide dependency, not a Trovato-specific problem. Pin `wasmtime = "28"` in Cargo.toml. Monitor wasmtime releases quarterly. Budget 2-4 weeks in year two for WASI migration when the Component Model stabilizes. The WIT interface abstraction means plugin source code won't change — only the compiled WASM target and host bindings need updating.

---

## 23. Getting Started: The First Vertical Spike

Don't build top-down. Build a thin vertical slice that touches every layer:

1. Axum server that serves a single page
2. One content type ("page") with one custom field ("field_subtitle")
3. One WASM plugin ("page") that defines the content type via `tap_item_info`
4. Item create/load that stores and retrieves the custom field in JSONB
5. One View ("recent_pages") that lists pages sorted by creation date
6. One `tap_item_view` that returns a JSON Render Element (not HTML)
7. The Render Tree pipeline converts the plugin's JSON output to HTML via Tera

If this works end-to-end, you've validated the architecture. Everything else is an extension.

```bash
# 1. Start the services
docker-compose up -d postgres redis

# 2. Build and run the Kernel
cargo run -- --database-url postgres://localhost/trovato \
             --redis-url redis://localhost

# 3. Compile the page plugin to WASM
cd plugins/page
cargo build --target wasm32-wasip1 --release
cp target/wasm32-wasip1/release/page.wasm ../../run/plugins/

# 4. Create a page
curl -X POST http://localhost:3000/item/add/page \
  -H "Content-Type: application/json" \
  -d '{"title": "Hello World", "fields": {"field_subtitle": {"value": "My first trovato page"}}}'

# 5. View it (Kernel loads WASM → WASM returns JSON → Kernel renders HTML)
curl http://localhost:3000/item/01936e3b-4f5a-7000-8000-000000000001

# 6. View the list
curl http://localhost:3000/recent-pages
```

That's the goal. Everything else follows from there.
