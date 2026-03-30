# Epic 14 (E): Performance Verification

**Tutorial Parts Affected:** None directly (performance infrastructure is invisible to tutorial narrative)
**Trovato Phase Dependency:** Phase 6 (Caching, Search, Hardening) — already complete
**BMAD Epic:** 44
**Status:** Not started
**Estimated Effort:** 2–3 weeks
**Dependencies:** None (independent)
**Blocks:** None

---

## Narrative

*Performance is not optimization. Optimization is what you do when something is slow. Performance-as-architecture is designing so that slow never happens in the first place — and when it tries to, the system tells you before your users do.*

Trovato's performance foundation is solid. Two-tier caching (Moka L1 + Redis L2) with tag-based invalidation. Server-side rendering (no client-side JS framework tax). SeaQuery-parameterized queries (no ORM N+1 traps by default). Connection pooling. Queue system for background work. Pagination on all listings.

What's missing are the guardrails that prevent plugins from accidentally creating performance problems, and the observability that catches problems before they reach users:

1. **No Gather relationship depth limit.** A plugin can write a Gather query that traverses deep relationship chains, generating unbounded JOINs. The query builder happily compiles it. This is the query-side equivalent of no max `per_page` — both let plugins create accidentally expensive operations.

2. **No query profiler.** Slow queries are invisible until users complain. A middleware that logs queries exceeding a configurable threshold (default 100ms) is the simplest possible observability — no external monitoring required.

3. **No asset versioning.** Static files (`/static/css/theme.css`, `/static/js/trovato.js`) served without content hashes. Browsers cache them, but cache invalidation on deploy requires users to hard-refresh. Content-hashed filenames enable immutable cache headers (cache forever — the URL changes when the content changes).

4. **No default `loading="lazy"` on images.** The render pipeline generates `<img>` tags without `loading="lazy"`. One Ritrovo template (`query--ritrovo.all_speakers.html`) adds it manually, but it should be the default.

5. **No N+1 audit of Gather relationships.** The Gather query builder uses JOINs for category relationships, but the full relationship loading path needs verification — especially for custom plugin-defined relationships.

**Before this epic:** Solid caching and query foundations with no guardrails against plugin-created performance problems and no query-level observability.

**After this epic:** Gather queries have depth limits. Slow queries are logged. Static assets use content-hashed filenames for immutable caching. Images lazy-load by default. Heavy operations verified to go through queues.

---

## Kernel Minimality Check

| Change | Why not a plugin? |
|---|---|
| Gather relationship depth limit | Query engine is kernel — plugins define queries, kernel executes them |
| Query profiler middleware | Middleware is kernel infrastructure |
| Asset versioning | Static file serving is kernel |
| `loading="lazy"` default | Render pipeline is kernel |
| N+1 audit | Kernel query execution path |

All changes are infrastructure-level. Performance *monitoring dashboards* and CDN integration are plugin territory.

---

## BMAD Stories

### Story 44.1: Gather Relationship Depth Limiting

**As a** kernel maintainer,
**I want** Gather queries limited to explicit relationship depth,
**So that** plugins cannot accidentally create queries with unbounded JOIN chains.

**Acceptance criteria:**

- [ ] Gather query definitions gain optional `max_depth: u8` parameter (default 1 — load direct relationships only)
- [ ] Depth 0: no relationship loading (item fields only)
- [ ] Depth 1: load direct relationships (item → category, item → author)
- [ ] Depth 2: load one level of nested relationships (item → category → parent_category)
- [ ] Maximum configurable depth: 3 (hard limit — deeper traversals must use multiple queries)
- [ ] Queries exceeding `max_depth` are truncated at the limit (no error — just don't load deeper relationships)
- [ ] Gather admin UI shows `max_depth` configuration
- [ ] `GATHER_MAX_RELATIONSHIP_DEPTH` env var overrides the hard limit (default 3)
- [ ] At least 2 integration tests: depth 0 (no JOINs), depth 2 (nested JOINs), depth exceeding limit (truncated)

**Implementation notes:**
- Modify `crates/kernel/src/gather/` query builder — add depth tracking during relationship resolution
- The extension registry already handles relationship loading — add depth parameter to extension query building
- This is a safety guardrail, not a performance optimization. The guard prevents unbounded query complexity.

---

### Story 44.2: Query Profiler Middleware

**As a** site operator,
**I want** slow database queries logged automatically,
**So that** I can identify performance problems before users report them.

**Acceptance criteria:**

- [ ] Middleware wraps database query execution and measures wall-clock duration
- [ ] Queries exceeding threshold logged at WARN level with: query text (parameterized — no sensitive values), duration in ms, route path, request ID
- [ ] `QUERY_SLOW_THRESHOLD_MS` env var (default 100ms)
- [ ] Queries exceeding 5x threshold logged at ERROR level (e.g., >500ms at default)
- [ ] Profiler is a compile-time feature flag (`--features query-profiler`) — zero overhead when disabled
- [ ] When enabled, adds `Server-Timing: db;dur=X` response header with total DB time per request
- [ ] Profiler does NOT log in tests (would be noisy) unless `QUERY_PROFILER_IN_TESTS=true`
- [ ] At least 1 integration test: verify slow query logging triggers (use a `pg_sleep()` query)

**Implementation notes:**
- Add `crates/kernel/src/middleware/query_profiler.rs`
- Wrap `PgPool::acquire()` or `sqlx::query()` execution — measure elapsed time
- `Server-Timing` header is a standard browser DevTools feature — shows DB time in the Network tab
- Feature flag avoids any runtime cost in production when not needed

---

### Story 44.3: Static Asset Content Hashing

**As a** site operator deploying updates,
**I want** static assets served with content-hashed filenames,
**So that** browsers cache assets immutably and always load the correct version after a deploy.

**Acceptance criteria:**

- [ ] At startup, kernel scans `static/` directory and computes SHA-256 hash of each file's contents
- [ ] Asset manifest maps original paths to hashed paths: `static/css/theme.css` → `static/css/theme.a1b2c3d4.css`
- [ ] Template helper `{{ asset_url("css/theme.css") }}` resolves to the hashed path
- [ ] Hashed assets served with `Cache-Control: public, max-age=31536000, immutable`
- [ ] Non-hashed paths (`/static/css/theme.css`) still work as fallback (302 redirect to hashed version, or serve directly with short cache)
- [ ] `base.html` updated to use `{{ asset_url("css/theme.css") }}` instead of hardcoded `/static/css/theme.css`
- [ ] Asset manifest regenerated on startup (not at build time — works with any deployment method)
- [ ] At least 1 integration test: verify hashed URL resolves, verify cache headers on hashed asset

**Implementation notes:**
- Add asset manifest generation to `crates/kernel/src/routes/static_files.rs` (or new `asset.rs`)
- Register `asset_url` as a Tera function or filter
- Hashing at startup is fast (typically <10ms for all static files)
- Only hash files in `static/` — uploaded files in `uploads/` are already unique (UUID-prefixed)

---

### Story 44.4: Default Lazy Loading on Images

**As a** site visitor on a slow connection,
**I want** images below the fold to load lazily,
**So that** the page becomes interactive faster and I don't download images I never scroll to.

**Acceptance criteria:**

- [ ] Render pipeline adds `loading="lazy"` as default attribute on all `<img>` elements it generates
- [ ] Block type rendering (`block--image.html`) includes `loading="lazy"` on `<img>`
- [ ] Image render elements in Rust fallback path include `loading="lazy"`
- [ ] First image in content (above the fold) should NOT have `loading="lazy"` — use `loading="eager"` for the first image to avoid delaying LCP (Largest Contentful Paint)
- [ ] Heuristic: first `<img>` in content body gets `loading="eager"`, all subsequent get `loading="lazy"`
- [ ] Plugin-generated images via `ElementBuilder` get `loading="lazy"` by default (can be overridden with `.attr("loading", "eager")`)
- [ ] Existing template `query--ritrovo.all_speakers.html` already has `loading="lazy"` — verify no duplication
- [ ] At least 1 integration test: render page with multiple images, verify lazy/eager attributes

**Implementation notes:**
- Modify `crates/kernel/src/theme/render.rs` — add `loading` attribute to image elements
- Modify `templates/elements/block--image.html` — add `loading="lazy"`
- Track image index during render to set first image as eager
- `loading="lazy"` is a web standard with broad browser support (all modern browsers)

---

### Story 44.5: Heavy Operation Queue Verification

**As a** kernel maintainer,
**I want** all computationally expensive operations verified to run through the queue system,
**So that** request handlers return quickly and heavy work doesn't block the event loop.

**Acceptance criteria:**

- [ ] Audit and document which operations go through queues vs. run inline:
  - Image style generation → verify queued (image_styles plugin)
  - Webhook delivery → verify queued (webhooks plugin — currently stub)
  - Search index rebuild → verify queued (trovato_search plugin via tap_cron)
  - Bulk stage publishing → verify transactions are bounded in size
  - Email sending → verify queued (if email is implemented)
  - AI requests → verify these run async, not blocking request handlers
- [ ] Any operation found running inline that should be queued gets a tracking issue created
- [ ] Document the queue architecture in operational docs: which queues exist, what workers process them, how to monitor queue depth
- [ ] Verify queue worker `tap_queue_worker` has timeout protection (already has 150s deadline — confirm)
- [ ] No changes required if all operations are already correctly queued — this is a verification story

**Implementation notes:**
- Audit `crates/kernel/src/` for inline heavy operations
- Audit plugin `tap_cron` and `tap_queue_worker` implementations
- This is primarily a verification and documentation story
- If gaps are found, create follow-up stories (not part of this epic's scope)

---

## Plugin SDK Changes

None. All changes are kernel-internal (query engine, middleware, static file serving, render pipeline).

---

## Design Doc Updates (shipped with this epic)

| Doc | Changes |
|---|---|
| `docs/design/Design-Query-Engine.md` | Add "Relationship Depth Limiting" section. Document max_depth parameter and its behavior. |
| `docs/design/Design-Infrastructure.md` | Add "Query Profiling" section. Add "Asset Versioning" section. Document Server-Timing header. Update queue architecture documentation with verification results from Story 44.5. |
| `docs/design/Design-Render-Theme.md` | Document `loading="lazy"` default on images. Document `asset_url()` template function. |

---

## Tutorial Impact

| Tutorial Part | Sections Affected | Nature of Change |
|---|---|---|
| `part-03-look-and-feel.md` | Template asset references | If `base.html` usage examples show `<link href="/static/css/theme.css">`, update to `{{ asset_url("css/theme.css") }}` syntax |

This is a minor change. Performance infrastructure is mostly invisible to the tutorial.

---

## Recipe Impact

Recipe for Part 3 needs minor update if asset URL syntax changes in template examples. Run `docs/tutorial/recipes/sync-check.sh` and update hashes.

---

## Screenshot Impact

None. Performance changes are invisible in screenshots.

---

## Config Fixture Impact

Gather query YAML definitions in `docs/tutorial/config/` may benefit from explicit `max_depth: 1` in relationship definitions, but this is optional since 1 is the default.

---

## Migration Notes

**Database migrations:** None. All changes are code-level.

**Breaking changes:** None directly. Gather queries with deep relationship chains may return fewer relationship levels than before (truncated at depth limit). This is intentional — queries that relied on unbounded depth were bugs.

**Upgrade path:** No action required. Existing Gather queries default to `max_depth: 1`. If a query needs deeper relationships, add explicit `max_depth: 2` or `max_depth: 3` to its definition.

---

## What's Deferred

- **CDN integration** — Plugin. The kernel provides content-hashed URLs; a CDN plugin rewrites them to CDN origins.
- **Advanced monitoring dashboard** — Plugin. The kernel provides `Server-Timing` headers and slow query logs; a monitoring plugin aggregates and visualizes.
- **Database connection pool tuning** — Operational concern, not code. Current 10-connection default is fine for most deployments.
- **HTTP/2 server push** — Future enhancement. Requires HTTPS and Axum configuration.
- **Image compression/WebP conversion** — Plugin territory (image_styles plugin).
- **Response compression (gzip/brotli)** — Middleware. Could be added here but is typically handled by a reverse proxy (nginx) in production.
- **Distributed tracing (OpenTelemetry)** — Future epic. Structured logging is the current approach.

---

## Related

- [Design-Query-Engine.md](../design/Design-Query-Engine.md) — Gather query builder
- [Design-Infrastructure.md](../design/Design-Infrastructure.md) — Caching, queues, search
- [Design-Render-Theme.md](../design/Design-Render-Theme.md) — Render pipeline and templates
- [Epic C (12): Security Hardening](epic-12-security.md) — Max page size story (42.5) complements relationship depth limiting
