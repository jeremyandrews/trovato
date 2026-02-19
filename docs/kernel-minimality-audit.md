# Kernel Minimality Audit

**Date:** 2026-02-19
**Governing Principle:** The core kernel enables. Plugins implement. If it's a feature, it's a plugin. If it's infrastructure that plugins depend on, it's Kernel.

---

## 1. Correctly-Placed Kernel Components

These subsystems are infrastructure that plugins depend on. They belong in the kernel.

### 1a. Plugin System (`plugin/`, 9 files)

**Verdict: Correctly placed — foundational infrastructure.**

WASM compilation, dependency resolution, lifecycle management, migration execution, and enable/disable gates. Every plugin depends on this subsystem to exist. The plugin gate pattern (`plugin_gate!` macro + `gated_plugin_routes`) is a clean separation mechanism.

### 1b. Tap System (`tap/`, 4 files)

**Verdict: Correctly placed — core extension mechanism.**

`TapDispatcher` invokes plugin hooks in weight order, `TapRegistry` collects handler registrations, `RequestState` provides per-request context to plugins. This is the spine of the plugin architecture — 14 tap points are actively invoked, 8+ more declared for future use.

### 1c. Content Management (`content/`, 8 files)

**Verdict: Correctly placed — foundational content infrastructure.**

`ContentTypeRegistry` collects types from `tap_item_info`. `ItemService` orchestrates CRUD with tap invocations (`tap_item_insert`, `tap_item_update`, `tap_item_delete`, `tap_item_access`, `tap_item_view`). All 7 content-defining plugins depend on this subsystem. `BlockTypeRegistry` and `BlockRenderer` are needed by any plugin using rich content. `FilterPipeline` is security infrastructure (XSS prevention). `FormBuilder` auto-generates admin UI from content type definitions.

### 1d. Gather/Query Engine (`gather/`, 7 files)

**Verdict: Correctly placed — declarative query infrastructure.**

`GatherService` executes declarative queries that any plugin can define. `GatherQueryBuilder` generates SQL via SeaQuery. `GatherExtensionRegistry` allows plugins to provide custom filters/sorts/relationships via `tap_gather_extend`. This is the Trovato equivalent of a query API — plugins define what to query, the kernel executes it.

**Note on CategoryService coupling:** `GatherService` depends on `CategoryService` for hierarchical filtering. This coupling is discussed in Section 3a.

### 1e. Theme Engine (`theme/`, 3 files)

**Verdict: Correctly placed — rendering infrastructure.**

Tera template engine with template suggestion resolution. Any plugin providing content needs the theme engine to render it. `RenderElement` → HTML conversion is used by content display across all content types.

### 1f. Routes/HTTP Handlers (`routes/`, 30 files)

**Verdict: Correctly placed — HTTP dispatch layer.**

Route handlers translate HTTP requests into service calls. The `gated_plugin_routes()` function cleanly separates plugin-specific routes (categories, comments, content_locking, image_styles, oauth2) with request-time enablement checks. Core routes (auth, admin dashboard, item CRUD, gather, install, health) are always-on infrastructure.

### 1g. Models (`models/`, 15 files)

**Verdict: Correctly placed — data schema layer.**

Database models for items, users, roles, categories, comments, languages, stages, tiles, menu links, URL aliases, API tokens, password resets, and site config. These define the schema that plugins operate on. The item model's JSONB field storage is the universal extension point.

### 1h. Host Functions (`host/`, 8 files)

**Verdict: Correctly placed — plugin IPC layer.**

WASM host functions provide the kernel API to plugins: database queries, item CRUD, cache operations, user/permission checks, request context, and logging. This is the contract between kernel and plugins.

### 1i. Middleware (`middleware/`, 8 files)

**Verdict: Correctly placed — request processing infrastructure.**

API token auth, bearer auth, install check, language negotiation, path alias resolution, rate limiting. These run on every request before route handlers.

**Note on redirect middleware:** The redirect middleware (`middleware/redirect.rs`) early-returns when the redirects plugin is disabled. The `RedirectCache` is conditionally instantiated (`Option<Arc<>>`) only when `is_plugin_enabled("redirects")`. See Section 3c (completed).

### 1j. Form System (`form/`, 5 files)

**Verdict: Correctly placed — admin UI infrastructure.**

Declarative forms with validation, AJAX support, and CSRF protection. `tap_form_alter` lets plugins modify forms. Any content type needs forms for admin editing.

### 1k. Config Storage (`config_storage/`, 4 files)

**Verdict: Correctly placed — configuration persistence layer.**

Unified abstraction (`dyn ConfigStorage`) for persisting content types, search config, categories, tags, variables, and languages. The trait abstraction supports both direct and stage-aware storage backends.

### 1l. Cache Layer (`cache/`, 1 file)

**Verdict: Correctly placed — performance infrastructure.**

Two-tier cache (Moka L1 + Redis L2) with tag-based invalidation. Exposed to plugins via host function cache API.

### 1m. Batch Operations (`batch/`, 3 files)

**Verdict: Correctly placed — background job infrastructure.**

Long-running task tracking. Any plugin-triggered bulk operation (reindex, migrate, import) needs this.

### 1n. File Management (`file/`, 3 files)

**Verdict: Correctly placed — file I/O infrastructure.**

`FieldType::File` references file UUIDs. Any plugin defining file fields depends on `FileService` for upload, validation, and storage (local/S3). Block editor image uploads also use this.

### 1o. Search Service (`search/`, 1 file)

**Verdict: Correctly placed — search infrastructure.**

PostgreSQL tsvector full-text search with GIN indexes. The `search_vector` column is baked into the item model. All content types benefit from search without opting in. Extraction would require plugins to modify item schema, which violates the kernel-owns-schema principle.

### 1p. Stage/Publishing (`stage/`, 1 file)

**Verdict: Correctly placed — content workflow infrastructure.**

Atomic ordered-phase publishing with conflict detection. Stages affect items, config, and categories — cross-cutting infrastructure that plugins shouldn't own.

### 1q. Cron/Scheduled Tasks (`cron/`, 3 files)

**Verdict: Correctly placed — background execution infrastructure.**

Distributed cron with Redis-based locking. The cron runner uses `set_plugin_services()` injection to optionally include plugin-specific tasks (content lock cleanup, audit log cleanup). After built-in tasks complete, the cron runner dispatches `tap_cron` via `TapDispatcher` to all plugins that implement the hook, enabling plugin-defined scheduled work. Clean optional dependency pattern.

### 1r. Menu System (`menu/`, 2 files)

**Verdict: Correctly placed — route and navigation infrastructure.**

`MenuRegistry` collects `tap_menu` definitions from plugins — this is the discovery mechanism for plugin routes. `MenuLink` model stores user-created navigation links (admin-managed data).

### 1s. Permissions (`permissions.rs`)

**Verdict: Correctly placed — access control infrastructure.**

`PermissionService` with DashMap cache, admin override, and role-based checking. Plugins register permissions via `tap_perm`; the kernel enforces them.

### 1t. Metrics (`metrics/`, 1 file)

**Verdict: Correctly placed — observability infrastructure.**

Prometheus counters/histograms for HTTP requests, tap durations, DB queries, cache hits/misses, rate limits. One-way data flow (kernel records, ops team reads). Not pluggable — plugins don't need to interact with metrics, and kernel metrics are foundational to operations.

### 1u. Session (`session.rs`), Auth (`lockout.rs`), Database (`db.rs`), Config (`config.rs`), State (`state.rs`), Error (`error.rs`)

**Verdict: All correctly placed — core runtime infrastructure.**

---

## 2. Extraction Candidates

These subsystems implement feature logic that lives in the kernel but could be extracted to plugins. Listed by extraction priority.

### 2a. Redirect Service — partial extraction, remainder stays in kernel

**Current state:** `services/redirect.rs` + `middleware/redirect.rs` + `RedirectCache` in AppState (conditional). The `redirects` plugin provides permissions and menu definitions. The middleware and cache are now conditional on plugin enablement. Dead code (`create_redirect_for_alias_change`) removed.

**Completed:**
- ✅ `RedirectCache` is `Option<Arc<>>` in AppState, instantiated only when `is_plugin_enabled("redirects")`
- ✅ Redirect middleware early-returns when cache is `None` (before language extraction for efficiency)
- ✅ Removed dead `create_redirect_for_alias_change()` function (defined but never called)

**Why full extraction is not practical:**
- Redirect middleware lookup is **hot-path** (every non-system request). WASM host function call overhead per request is unacceptable for this path.
- Admin CRUD routes are small and already handled by kernel routes pointed to by the plugin's `tap_menu`.
- No `tap_route` mechanism exists for plugins to handle their own HTTP routes yet.

**Verdict:** Current state is the final state. Service stays in kernel, gated behind plugin enablement.

### 2b. Image Style Service — keep in kernel (gated)

**Current state:** `services/image_style.rs` + `routes/image_style.rs`. Routes are already gated behind `gate_image_styles`. The `image_styles` plugin only provides permissions and menus. `ImageStyleService` is `Option<Arc<>>` in AppState, instantiated only when the plugin is enabled.

**Previously classified as "Extract"** with blocker "File path host function." However, unlike the previous extractions (webhook, translation, scheduled publishing) — which were all dead code with zero kernel callers — the image style service has an **active HTTP route** at `GET /files/styles/{style_name}/{*path}` that serves image derivatives on demand.

**Why full extraction is not practical:**
1. **No `tap_route` mechanism** — plugins cannot serve HTTP routes. The derivative endpoint must live in the kernel's route layer.
2. **WASM can't do image processing** — the `image` crate requires std filesystem access and CPU-intensive pixel operations (resize, crop, scale). WASM's sandboxed linear memory and lack of std I/O make this infeasible.
3. **WASM can't do filesystem I/O** — derivative caching requires reading source files and writing generated derivatives to disk. No host function exists for raw filesystem access, and adding one would violate the sandbox security model.

**Already properly gated:**
- `ImageStyleService` is `Option<Arc<>>` in `AppStateInner`
- Route is behind `plugin_gate!("image_styles")` middleware
- Listed in `GATED_ROUTE_PLUGINS` in `plugin/gate.rs`

**Verdict:** Current state is the final state. This matches the redirect service conclusion (Section 2a) — hot-path kernel functionality that can't move to WASM. Service stays in kernel, gated behind plugin enablement.

### 2c. Scheduled Publishing Service → `scheduled_publishing` plugin — ✅ EXTRACTED

**Previous state:** `services/scheduled_publishing.rs` + hardcoded cron task. The service used `field_publish_on`/`field_unpublish_on` JSONB fields on items.

**Extraction completed:**
- ✅ Kernel-side `ScheduledPublishingService` deleted (`services/scheduled_publishing.rs`)
- ✅ Hardcoded cron task removed from `CronTasks` and `CronService::run()`
- ✅ Plugin implements `tap_cron` using `host::execute_raw()` for publish/unpublish SQL
- ✅ Plugin `.info.toml` updated to declare `tap_cron` in implements list
- ✅ `set_plugin_services()` signature simplified (no longer takes scheduled_publishing arg)
- ✅ `AppStateInner` no longer carries `scheduled_publishing` field or accessor

**Pattern established:** This is the reference extraction for moving kernel cron tasks to plugin `tap_cron` handlers via DB host functions.

### 2d. Translation Service → `content_translation` plugin — ✅ EXTRACTED

**Previous state:** `services/translation.rs` + `services/po_parser.rs` + `services/translated_config.rs`. The `content_translation` plugin only provided permissions and menus.

**Extraction completed:**
- ✅ Kernel-side `TranslationService` deleted (`services/translation.rs`)
- ✅ Kernel-side `TranslatedConfigStorage` deleted (`services/translated_config.rs`)
- ✅ Kernel-side PO parser deleted (`services/po_parser.rs`)
- ✅ `AppStateInner` no longer carries `translation` field or accessor
- ✅ No kernel routes, middleware, or cron tasks referenced these services (zero callers)

**Note:** The `content_translation` plugin already owns the `item_translation` table migration. Translation CRUD operations will be implemented in the plugin when admin routes are built via `tap_route` or host DB functions — that's a separate future task.

### 2e. Webhook Service → `webhooks` plugin — ✅ EXTRACTED

**Previous state:** `services/webhook.rs` + hardcoded cron task for delivery processing. The service provided webhook CRUD, HMAC-signed delivery, SSRF prevention, exponential-backoff retry, and AES-256-GCM secret encryption.

**Extraction completed:**
- ✅ Kernel-side `WebhookService` deleted (`services/webhook.rs`)
- ✅ Hardcoded cron task removed from `CronTasks` and `CronService::run()`
- ✅ `set_plugin_services()` signature simplified (no longer takes webhooks arg)
- ✅ `AppStateInner` no longer carries `webhooks` field, instantiation block, or accessor
- ✅ HKDF key derivation block and `# Panics` doc removed from `AppState::new()`
- ✅ Zero callers existed in kernel — `state.webhooks()` was never called, `dispatch()` was never invoked, delivery queue was always empty

**Note:** The `webhooks` plugin already owns the `webhook` and `webhook_delivery` table migrations. When webhook functionality is needed, the plugin will implement it via `tap_item_*` hooks (to queue events) and `tap_cron` (to process deliveries).

### 2f. Email Service — keep in kernel

**Current state:** `services/email.rs`. Called from `routes/password_reset.rs`. `EmailService` is `Option<Arc<>>` in `AppStateInner`, instantiated only when `SMTP_HOST` environment variable is set. Password reset gracefully handles `None` (logs warning, returns success to avoid user enumeration).

**Previously classified as "Extract"** with a `tap_send_email` approach. However, the same WASM sandbox constraint that blocks image styles (Section 2b) also blocks email:

1. **WASM can't do network I/O** — SMTP requires TCP connections with TLS negotiation. WASM's sandboxed environment has no socket API. Unlike HTTP webhooks (which could theoretically use a host function), SMTP is a stateful multi-step protocol unsuitable for host function wrapping.
2. **Active caller** — unlike webhook, translation, and scheduled publishing (all dead code with zero callers that were simply deleted), email has an active caller in `routes/password_reset.rs`.
3. **No plugin to extract to** — email is configuration-conditional (`SMTP_HOST` env var), not plugin-gated. There is no "email" plugin, and creating one would add complexity without benefit.

**Already properly conditional:**
- `EmailService` is `Option<Arc<>>` in `AppStateInner`
- Created only when `SMTP_HOST` is set (config-conditional, not plugin-gated)
- Password reset handles `None` gracefully

**Verdict:** Keep as configuration-conditional auth infrastructure. This is not "Keep (gated)" — there is no plugin gate because email is not a plugin feature. It's infrastructure for password reset, which is core auth.

### 2g. Audit Service — keep in kernel

**Current state:** `services/audit.rs` + cron cleanup task. Initially classified as extractable, but:

**Why keep:** Audit logging is compliance infrastructure. While currently only called from cron cleanup (the recording happens elsewhere via direct DB writes), the audit service provides retention management. The kernel's `tap_item_*` hooks log audit events. Moving audit to a plugin risks compliance gaps if the plugin is disabled.

**Revised verdict:** Keep audit service in kernel. It's security/compliance infrastructure — disabling it shouldn't be possible without deliberate action beyond plugin management.

### 2h. Content Lock Service — keep in kernel

**Current state:** `services/content_lock.rs` + `routes/lock.rs` (gated). Already properly gated behind `gate_content_locking`.

**Why keep:** Content locking prevents data loss from concurrent editing. It's infrastructure for data integrity. The routes are already gated, and the service is optional — good pattern as-is.

**Revised verdict:** Current placement is correct. The service is optional (`Option<Arc<ContentLockService>>`), gated at the route level, and handles a cross-cutting concern (data integrity).

---

## 3. Kernel Interface Modifications for Extraction

### 3a. CategoryService Coupling with GatherService

`GatherService` takes `Arc<CategoryService>` as a constructor dependency for hierarchical filtering. This means `CategoryService` cannot be naively extracted without breaking gather queries.

**Resolution options:**
1. **Keep CategoryService in kernel** — Categories are a fundamental content classification system. The hierarchical query capability is infrastructure. The category CRUD routes can be gated (they already are), but the service stays.
2. **Extract with trait abstraction** — Define a `CategoryProvider` trait in the kernel, implement it in the categories plugin via host functions. GatherService depends on the trait, not the concrete service.

**Recommendation:** Option 1 — CategoryService is infrastructure. The `categories` plugin provides UI/permissions; the kernel provides the query and storage layer. This matches the existing pattern where the plugin declares permissions and the kernel implements the service.

### 3b. Activate `tap_cron` Dispatch — ✅ COMPLETED

`tap_cron` is now dispatched during each cron cycle, after all built-in tasks complete.

**Changes made:**
- `CronService` holds `Option<Arc<TapDispatcher>>` via `set_tap_dispatcher()`
- After built-in tasks, dispatches `tap_cron` with `{"timestamp": <unix_ts>}` payload
- Dispatch is wrapped in a timeout (half the lock TTL) to prevent runaway plugins
- Handler count comparison detects and warns on per-plugin failures
- Each plugin result is logged and added to the cron run task list
- `CronInput` type added to `plugin-sdk/src/types.rs` documenting the input contract
- Wired at startup in `AppState::new()` alongside `set_plugin_services()`

### 3c. Conditional Redirect Middleware — ✅ COMPLETED

`RedirectCache` is now conditional on redirects plugin enablement.

**Changes made:**
- `redirect_cache` field in `AppStateInner` is `Option<Arc<RedirectCache>>`
- Instantiated only when `enabled_set.contains("redirects")`
- Redirect middleware early-returns when cache is `None` (checked before language extraction for efficiency)
- Conditionality follows the `Option<Arc<>>` pattern used by other optional services, not `GATED_ROUTE_PLUGINS` (redirects has no kernel-side routes to gate)

### 3d. File Path Host Function — no longer needed

Previously planned for `image_styles` plugin extraction. Since image style service is now classified as **Keep (gated)** (Section 2b), this host function has no remaining use case. If a future plugin needs file path resolution, this can be revisited.

### 3e. Email Abstraction — no longer needed

Previously planned `tap_send_email` tap for email plugin extraction. Since email service is now classified as **Keep** (Section 2f), this tap has no remaining use case. WASM cannot perform SMTP network I/O, and email is configuration-conditional auth infrastructure with no plugin to extract to.

---

## 4. Versioning Scheme for API Stability

### Plugin SDK Version Contract

The plugin-SDK crate version (`trovato-sdk`) should follow semantic versioning with these stability guarantees:

**Stable (covered by semver MAJOR):**
- Host function signatures (WASM import names, parameter counts, return types)
- Error code constants in `host_errors.rs`
- SDK type definitions (`Item`, `ContentTypeDefinition`, `FieldDefinition`, `FieldType`, etc.)
- Tap function signatures (input/output types)
- `#[plugin_tap]` / `#[plugin_tap_result]` macro behavior

**Semi-stable (covered by semver MINOR):**
- New tap points (adding `tap_cron`, etc.)
- New host functions
- New SDK types
- New fields on existing types (additive only)
- New `FieldType` variants

**Internal (not covered by semver — kernel-only):**
- Service implementations (how kernel executes taps)
- Database schema (migrations are kernel-internal)
- Route handler implementations
- Middleware ordering
- Cron task scheduling

### Proposed Version Format

```
trovato-sdk 1.MINOR.PATCH
```

- **MAJOR = 1** until the first breaking change to host function signatures or core types
- **MINOR** increments when new taps, host functions, or SDK types are added
- **PATCH** increments for documentation, bug fixes, and non-breaking changes

### Compatibility Matrix

| SDK Version | Kernel Version | Status |
|-------------|---------------|--------|
| 1.0.x | Current | Baseline |
| 1.1.x | +tap_cron | Additive |
| 2.0.x | Breaking host function change | Major bump |

Plugins declare `sdk_version = "1.0"` in `.info.toml`. The kernel checks compatibility at plugin load time and refuses to load plugins compiled against an incompatible SDK major version.

---

## 5. Summary Table

| Subsystem | Classification | Verdict | Notes |
|-----------|---------------|---------|-------|
| Plugin system | Infrastructure | Keep | Foundational |
| Tap system | Infrastructure | Keep | Core extension mechanism |
| Content management | Infrastructure | Keep | All content plugins depend on it |
| Gather/query engine | Infrastructure | Keep | Declarative query layer |
| Theme engine | Infrastructure | Keep | Rendering layer |
| Routes (core) | Infrastructure | Keep | HTTP dispatch |
| Models | Infrastructure | Keep | Data schema |
| Host functions | Infrastructure | Keep | Plugin IPC |
| Middleware (core) | Infrastructure | Keep | Request processing |
| Form system | Infrastructure | Keep | Admin UI infrastructure |
| Config storage | Infrastructure | Keep | Configuration persistence |
| Cache layer | Infrastructure | Keep | Performance |
| Batch operations | Infrastructure | Keep | Background jobs |
| File management | Infrastructure | Keep | `FieldType::File` dependency |
| Search service | Infrastructure | Keep | Baked into item schema |
| Stage/publishing | Infrastructure | Keep | Content workflow |
| Cron runner | Infrastructure | Keep | Background execution |
| Menu system | Infrastructure | Keep | Plugin route discovery |
| Permissions | Infrastructure | Keep | Access control |
| Metrics | Infrastructure | Keep | Observability |
| Session/Auth/DB | Infrastructure | Keep | Core runtime |
| Category service | Infrastructure | Keep | GatherService dependency |
| Audit service | Infrastructure | Keep | Compliance (revised) |
| Content lock service | Infrastructure | Keep | Data integrity (gated) |
| **Redirect service** | **Feature** | **Keep (gated)** | Middleware + cache conditional ✅; hot-path prevents full WASM extraction |
| **Image style service** | **Feature** | **Keep (gated)** | `Option<Arc<>>` + `plugin_gate!`; WASM can't do image processing or filesystem I/O |
| **Scheduled publishing** | **Feature** | **Extracted ✅** | Now plugin `tap_cron` handler via `host::execute_raw()` |
| **Translation service** | **Feature** | **Extracted ✅** | Dead kernel code removed; plugin owns table + future CRUD |
| **Webhook service** | **Feature** | **Extracted ✅** | Dead kernel code removed; plugin owns tables + future event hooks |
| **Email service** | **Infrastructure** | **Keep** | Configuration-conditional auth infrastructure; WASM can't do SMTP |

---

## 6. Extraction Priority

| Priority | Service | Blocking Dependency | Effort |
|----------|---------|-------------------|--------|
| ~~1~~ | ~~Redirect service~~ | ~~Conditional middleware~~ ✅ resolved | ~~Low~~ — kept in kernel (hot-path) |
| ~~2~~ | ~~Scheduled publishing~~ | ~~Activate tap_cron~~ ✅ resolved | ~~Low~~ — **extracted** ✅ |
| ~~3~~ | ~~Image style service~~ | ~~File path host function~~ | ~~Medium~~ — kept in kernel (WASM can't do image processing/filesystem I/O) |
| ~~4~~ | ~~Webhook service~~ | ~~Activate tap_cron~~ ✅ resolved | ~~Medium~~ — **extracted** ✅ |
| ~~5~~ | ~~Translation service~~ | ~~Host function DB patterns~~ ✅ resolved | ~~Medium~~ — **extracted** ✅ |
| ~~6~~ | ~~Email service~~ | ~~tap_send_email + auth fallback~~ | ~~Medium~~ — kept in kernel (WASM can't do SMTP) |
