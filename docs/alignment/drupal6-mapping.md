# Drupal 6 Core Concept Mapping

Systematic mapping of Drupal 6 + CCK + Views subsystems to their Trovato equivalents. Each section shows D6 behavior, Trovato implementation, and alignment status.

**Legend:** Aligned = feature parity or better | Partial = core works, some D6 features missing | Missing = not implemented

**Scope:** "D6 parity" means Drupal 6 core + CCK + Views (the three modules installed on virtually every D6 site). Where D6 contrib modules (Pathauto, imagecache, i18n, etc.) are referenced, they are noted as contrib. Trovato is PostgreSQL-only; D6 supported MySQL, PostgreSQL, and SQLite via its database abstraction layer. This is an intentional divergence documented in `intentional-divergences.md`.

---

## 1. Content System (Node -> Item)

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| `node` table | `item` table | Aligned |
| `node_revisions` | `item_revision` | Aligned |
| `node_type` | `item_type` | Aligned |
| `node.nid` (auto-increment) | `item.id` (UUIDv7) | Aligned (intentional divergence) |
| `node.uid` | `item.author_id` | Aligned |
| `node.status` (0/1) | `item.status` (i16) | Aligned |
| `node.title` | `item.title` | Aligned |
| `node.promote` / `node.sticky` | `item.promote` / `item.sticky` | Aligned |
| `node.created` / `node.changed` | `item.created` / `item.changed` | Aligned |
| `node.language` | `item.language` | Aligned |
| `node_access` grants table | `tap_item_access` (Deny/Grant/Neutral) | Partial (see notes) |
| Per-field revision tables | Single `fields` JSONB per revision | Aligned (intentional divergence) |

**Key files:**
- `crates/kernel/src/models/item.rs` -- Item, ItemRevision, CreateItem, UpdateItem
- `crates/kernel/src/content/item_service.rs` -- CRUD, access checking, revision management
- `crates/kernel/src/content/type_registry.rs` -- ContentTypeRegistry, type/field definitions

**Notes:** Every mutation creates a new `item_revision` row. Fields are stored as a single JSONB blob per revision rather than D6's per-field revision tables. Access control uses tap-based aggregation (Deny > Grant > Neutral) instead of D6's `node_access` grants table. This is a meaningful performance trade-off: D6's grants table pre-computed access as a simple SQL JOIN (O(1) per query), while Trovato's tap-based approach dispatches WASM taps per item per request. For listings of many items, this could become a bottleneck if plugins implement expensive access logic.

---

## 2. CCK / Field System (JSONB Fields)

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| `content_node_field` definitions | `item_type.fields` JSONB schema | Aligned |
| `content_node_field_instance` | Field config in type definition | Aligned |
| Per-type content tables (`content_type_*`) | Single `item.fields` JSONB column | Aligned (intentional divergence) |
| Field types: Text, Number, Nodereference | Text, Integer, Float, RecordReference | Aligned |
| Textarea / Long text | TextLong | Aligned |
| Boolean, Date, Email | Boolean, Date, Email | Aligned |
| File / Image field | File field type (via FileService) | Aligned |
| CCK Multigroup | Compound field type | Aligned |
| Field cardinality (unlimited) | JSONB array values | Aligned |
| Field validation | Form validation pipeline | Aligned |
| Field widget (form element) | Auto-generated from field type | Aligned |
| Field formatter (display) | RenderElement output | Aligned |

**Key files:**
- `crates/kernel/src/content/type_registry.rs` -- field type definitions (Text, TextLong, Integer, Float, Boolean, Date, Email, RecordReference, Compound)

**Notes:** D6's EAV (Entity-Attribute-Value) pattern required N+1 JOINs. Trovato stores all fields in a single JSONB column, eliminating this. Expression indexes on JSONB paths handle query performance. The Compound field type replaces CCK's multigroup concept.

---

## 3. Views -> Gather (Query Engine)

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| View definition | `QueryDefinition` | Aligned |
| Base table + relationships | Base table + joins | Aligned |
| Filter criteria | `GatherFilter` with `FilterOperator` | Aligned |
| Sort criteria | `GatherSort` | Aligned |
| Filter operators (=, <>, LIKE, etc.) | 18 operators including category hierarchy | Aligned |
| Contextual filters (arguments) | `ContextualValue` (CurrentUser, CurrentTime, UrlArg) | Aligned |
| Pager | `PagerConfig` with limit/offset | Aligned |
| Exposed filters (URL params) | Basic URL parameter filtering | Aligned |
| Exposed filter forms | Not yet implemented | Partial |
| Views UI (admin) | Not yet implemented | Missing |
| Display plugins (page/block/feed/attachment) | Not yet implemented | Missing |
| Relationships (JOINs) | SeaQuery JOIN support | Aligned |
| `hook_views_data` | `tap_gather_extend` | Aligned |
| Views caching | Cache layer integration | Aligned |
| JSONB field filtering | Automatic `fields->>'path'` extraction | Aligned |
| Stage-aware queries | CTE wrapping for stage filtering | Aligned |

**Key files:**
- `crates/kernel/src/gather/types.rs` -- QueryDefinition, GatherQuery, GatherResult, all filter/sort types
- `crates/kernel/src/gather/query_builder.rs` -- GatherQueryBuilder (SeaQuery-based SQL)

**Notes:** Gather is fully functional as a programmatic/config-driven query engine. The primary gap is the admin UI for building queries (planned in Epic 23). Filter operators include category-aware hierarchy operators (`HasTagOrDescendants`) that exceed D6 Views capabilities.

---

## 4. Taxonomy -> Categories

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| `vocabulary` table | `category` table | Aligned |
| `term_data` table | `tag` table | Aligned |
| `term_hierarchy` | `tag_hierarchy` | Aligned |
| Vocabulary machine name | `category.id` (string) | Aligned |
| Term weight | `tag.weight` | Aligned |
| Hierarchy modes (flat/single/multiple) | `category.hierarchy` (0/1/2) | Aligned |
| Term depth / tree building | Recursive CTE queries | Aligned |
| Term reference field | `RecordReference` field type | Aligned |
| Free tagging (autocomplete) | Not in core Form API | Missing |
| Term synonyms | Not implemented | Missing |
| `hook_taxonomy` | `tap_categories_term_*` taps | Aligned |
| Breadcrumb from term hierarchy | `get_ancestors` CTE | Aligned |

**Key files:**
- `crates/kernel/src/models/category.rs` -- Category, Tag, TagHierarchy; DAG queries

**Notes:** Trovato supports true DAG (directed acyclic graph) hierarchy with multiple parents, matching D6's vocabulary hierarchy mode 2. Recursive CTEs provide `get_ancestors`, `get_descendants`, `get_tag_and_descendant_ids`.

---

## 5. Hook System -> Tap System

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| `hook_nodeapi` (view/insert/update/delete) | `tap_item_view`, `tap_item_insert`, `tap_item_update`, `tap_item_delete` | Aligned |
| `hook_node_access` | `tap_item_access` | Aligned |
| `hook_perm` | `tap_perm` | Aligned |
| `hook_menu` | `tap_menu` | Aligned |
| `hook_form_alter` | `tap_form_alter` | Aligned |
| `hook_form_validate` | `tap_form_validate` | Aligned |
| `hook_form_submit` | `tap_form_submit` | Aligned |
| `hook_theme` | `tap_theme` | Aligned |
| `hook_preprocess_*` | `tap_preprocess_item` | Partial |
| `hook_cron` | `tap_cron` | Aligned |
| `hook_install` / `hook_enable` | `tap_install` / `tap_enable` | Aligned |
| `hook_disable` / `hook_uninstall` | `tap_disable` / `tap_uninstall` | Aligned |
| `hook_user` (login) | `tap_user_login` | Partial |
| `hook_user` (logout/register/update/delete) | Not implemented | Missing |
| `hook_init` / `hook_exit` | Not implemented | Missing |
| `hook_views_data` | `tap_gather_extend` | Aligned |
| `hook_update_N` (schema updates) | SQL migration system | Aligned (different mechanism) |
| `module_invoke_all` | `TapDispatcher::dispatch_all` | Aligned |
| Weight-based ordering | `TapRegistry` with weight sort | Aligned |
| `hook_queue_info` / `hook_queue_worker` | `tap_queue_info` / `tap_queue_worker` | Aligned |
| `hook_update_index` | `tap_item_update_index` | Aligned |

**Key files:**
- `crates/kernel/src/tap/dispatcher.rs` -- TapDispatcher, WASM invocation
- `crates/kernel/src/tap/registry.rs` -- TapRegistry, weight-based ordering

**Notes:** All major content lifecycle hooks are covered. The primary gap is the `hook_user` family -- only `tap_user_login` exists; logout, register, update, and delete user taps are missing. Per-request lifecycle hooks (`hook_init`/`hook_exit`) are intentionally omitted since middleware handles that concern.

---

## 6. Module System -> Plugin System (WASM)

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| `.info` file | `trovato.info.toml` manifest | Aligned |
| `.module` file (PHP) | `lib.rs` compiled to `.wasm` | Aligned |
| `.install` file | `migrations/*.sql` | Aligned |
| `hook_schema` | SQL migration files | Aligned |
| Module dependencies | `dependencies` in info.toml | Aligned |
| Module enable/disable | Plugin enable/disable with status tracking | Aligned |
| Module weight (execution order) | Tap weight per hook | Aligned |
| `module_invoke` / `module_invoke_all` | `TapDispatcher` dispatch | Aligned |
| `drupal_get_path('module', ...)` | Not needed (WASM is self-contained) | N/A |
| `hook_requirements` | Not implemented | Missing |
| Optional dependencies | Not supported (hard deps only) | Missing |
| `hook_update_N` | SQL migration system (numbered files) | Aligned |

**Key files:**
- `crates/kernel/src/plugin/runtime.rs` -- PluginRuntime, Wasmtime engine, WASI stubs
- `crates/kernel/src/plugin/info_parser.rs` -- PluginInfo, TapConfig, MigrationConfig
- `crates/plugin-sdk/` -- Rust SDK for plugin authors

**Notes:** WASM plugins are sandboxed with no filesystem or network access. The plugin SDK provides proc macros and type definitions. Wasmtime's pooling allocator enables ~5 us instantiation per request.

---

## 7. User / Auth / Permissions

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| `users` table | `user` table | Aligned |
| `users.uid` (auto-increment) | `user.id` (UUIDv7) | Aligned |
| `users.pass` (MD5 in D6) | `user.pass` (Argon2id) | Aligned (intentional divergence) |
| User 1 = superadmin | `user.is_admin` boolean | Aligned (intentional divergence) |
| `role` table | `role` table | Aligned |
| `permission` table | `role_permission` table | Aligned |
| `users_roles` | `user_role` table | Aligned |
| Anonymous user (uid=0) | Anonymous user (Uuid::nil) | Aligned |
| Session in database | Session in Redis | Aligned (intentional divergence) |
| `drupal_session` | tower-sessions + Redis | Aligned |
| `hook_user` (login) | `tap_user_login` | Aligned |
| User picture | Not a standard field | Missing |
| User blocking by IP | Not implemented | Missing |
| Password reset / one-time login | Routes and token model exist; email delivery pending | Partial |
| `user_access` function | `PermissionService::user_has_permission` | Aligned |
| Email sending (`drupal_mail`) | No email infrastructure (tokens logged, not emailed) | Missing |

**Key files:**
- `crates/kernel/src/models/user.rs` -- User, password verification
- `crates/kernel/src/permissions.rs` -- PermissionService, DashMap-cached lookups
- `crates/kernel/src/session.rs` -- Redis session store (fred + tower-sessions-redis-store)
- `crates/kernel/src/routes/password_reset.rs` -- password reset routes (token-based)
- `crates/kernel/src/routes/auth.rs` -- login, logout, registration routes

**Notes:** Permission checks use DashMap for fast in-memory lookups. Admin users bypass all permission checks. The `data` JSONB column on users allows arbitrary profile data storage. Password reset routes exist (`POST /user/password-reset`, `GET/POST /user/password-reset/{token}`) with HMAC-signed tokens, but tokens are currently logged rather than emailed -- no SMTP/email delivery infrastructure exists yet.

---

## 8. Form API

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| `drupal_get_form()` | Form builder + router | Aligned |
| Form element types | `ElementType` enum (15 types) | Aligned |
| `#required` / `#default_value` | `required` / `default_value` fields | Aligned |
| `#weight` ordering | `weight` field, BTreeMap ordering | Aligned |
| `#prefix` / `#suffix` | `prefix` / `suffix` fields | Aligned |
| CSRF token (`form_token`) | `csrf_token` field | Aligned |
| `form_build_id` | `form_build_id` (UUID) | Aligned |
| `hook_form_alter` | `tap_form_alter` | Aligned |
| Validation pipeline | `tap_form_validate` + processor | Aligned |
| Submission pipeline | `tap_form_submit` + processor | Aligned |
| AHAH/AJAX | `AjaxConfig` per element | Aligned |
| Form state cache | PostgreSQL `form_state_cache` | Aligned |
| Fieldset / Container | Fieldset, Container element types | Aligned |
| File upload element | File element type | Aligned |
| `#theme` per element | Fixed template per type | Partial |
| `#process` callbacks | Not implemented | Missing |
| Multi-step form wizard | Not in core | Missing |

**Key files:**
- `crates/kernel/src/form/types.rs` -- Form, FormElement, ElementType, AjaxConfig
- `crates/kernel/src/form/service.rs` -- validation and submission pipeline
- `crates/kernel/src/form/ajax.rs` -- AJAX form operations
- `crates/kernel/src/form/csrf.rs` -- CSRF token generation and verification

**Notes:** Trovato's Form API closely follows D6's declarative form model. AJAX support uses a similar event + wrapper pattern. The main divergence is template handling: D6 allowed per-element `#theme` overrides; Trovato uses fixed `elements/{type}.html` templates.

---

## 9. Theme / Render System

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| PHPTemplate engine | Tera template engine | Aligned (intentional divergence) |
| Template suggestions | Template suggestion arrays | Aligned |
| `hook_preprocess_*` | `tap_preprocess_item` | Partial |
| Theme registry | ThemeEngine with template resolution | Aligned |
| `drupal_render()` | `RenderTreeConsumer` | Aligned |
| Render arrays | `RenderElement` JSON trees | Aligned (intentional divergence) |
| Regions (left sidebar, content, etc.) | Slots | Aligned |
| `page.tpl.php` | `page.html` (Tera) | Aligned |
| `node.tpl.php` | `item.html`, `item--{type}.html` | Aligned |
| `block.tpl.php` | No tile templates or module | Missing |
| Sub-theme inheritance | Not implemented | Missing |
| `theme_get_setting()` | Not implemented | Missing |
| `#weight` ordering in render | `#weight` sorting in RenderTreeConsumer | Aligned |
| Text format rendering | `FilterPipeline` based on `#format` | Aligned |

**Key files:**
- `crates/kernel/src/theme/render.rs` -- RenderTreeConsumer, element-to-HTML conversion
- `crates/plugin-sdk/src/render.rs` -- RenderElement, ElementBuilder for plugins
- `crates/kernel/templates/` -- Tera templates

**Notes:** Plugins never output raw HTML. They build `RenderElement` JSON trees using the SDK's fluent builder API. The kernel converts these to HTML via Tera, preventing XSS. Preprocess taps are additive (return additions, not mutations) to prevent plugin clobber.

---

## 10. Text Formats / Filters

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| Filter formats (Filtered HTML, Full HTML) | Named formats (plain_text, filtered_html, full_html) | Aligned |
| `hook_filter` | `TextFilter` trait | Aligned |
| HTML tag allowlist | Regex-based blocklist | Partial |
| `filter_xss()` | `FilteredHtmlFilter` (regex) | Partial |
| URL filter | `UrlFilter` | Aligned |
| Line break filter | `NewlineFilter` | Aligned |
| HTML escape filter | `HtmlEscapeFilter` | Aligned |
| Per-role format permissions | Not implemented | Missing |
| Format admin UI | Not implemented | Missing |

**Key files:**
- `crates/kernel/src/content/filter.rs` -- FilterPipeline, TextFilter trait, built-in filters

**Notes:** The most significant gap is that `FilteredHtmlFilter` uses regex-based tag stripping rather than a proper HTML parser with allowlist (like D6's `filter_xss_admin`). An upgrade to ammonia/html5ever is planned. Per-role format permissions are also missing; any authenticated user can currently use any format.

---

## 11. Menu System

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| `hook_menu` definitions | `tap_menu` definitions | Aligned |
| Menu router (path -> callback) | Axum router + MenuRegistry | Aligned |
| Path parameters (`%node`) | `:param` syntax | Aligned |
| Menu tree hierarchy | Parent-child in MenuDefinition | Aligned |
| Menu weight ordering | Weight field | Aligned |
| Access callback / permission | Permission field per menu item | Aligned |
| Breadcrumbs from menu | Not auto-generated from hierarchy | Partial |
| Custom menu links (admin-created) | Not implemented | Missing |
| Primary / secondary links | Not implemented | Missing |
| Local tasks (tabs) | Not implemented | Missing |

**Key files:**
- `crates/kernel/src/menu/registry.rs` -- MenuRegistry, MenuDefinition, path matching

---

## 12. File Management

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| `files` table | `file_managed` table | Aligned |
| `file.uid` (owner) | `file_managed.owner_id` | Aligned |
| `file.status` (temporary/permanent) | `FileStatus` (Temporary/Permanent) | Aligned |
| `file.filepath` | `file_managed.uri` (scheme://path) | Aligned |
| File upload validation | Size + MIME type checks | Aligned |
| Temporary file cleanup (cron) | `cleanup_temp_files` cron task | Aligned |
| `file_usage` table (reference counting) | Not implemented | Missing |
| Private file system | Not implemented | Missing |
| Local file storage | `LocalFileStorage` | Aligned |
| S3 storage | `S3FileStorage` (stub, deferred) | Partial |
| Image processing | `ImageStyleService` (scale, crop, resize, desaturate) | Aligned |

**Key files:**
- `crates/kernel/src/file/service.rs` -- FileService, FileInfo, upload/delete/cleanup
- `crates/kernel/src/file/storage.rs` -- FileStorage trait, LocalFileStorage

**Notes:** Files use a URI scheme (`local://` or `s3://`) rather than bare paths. SVG is excluded from allowed MIME types to prevent stored XSS. The main gap is `file_usage` reference counting -- files are marked permanent/temporary but there is no tracking of which items reference which files.

---

## 13. Search

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| `search_index` table | `search_index` with tsvector column | Aligned |
| `search_dataset` | Folded into tsvector | Aligned |
| `hook_update_index` | `tap_item_update_index` | Aligned |
| `hook_search` (execute) | `SearchService::search()` | Aligned |
| Field-weight configuration | `search_field_config` (A-D weights) | Aligned |
| `search_excerpt()` | `ts_headline` with `<mark>` tags | Aligned |
| Search ranking | `ts_rank` | Aligned |
| User/comment search | Not implemented | Missing |
| Search module per content type | Not implemented | Missing |

**Key files:**
- `crates/kernel/src/search/mod.rs` -- SearchService, indexing, querying

---

## 14. Cron / Queue

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| `hook_cron` | `tap_cron` | Aligned |
| `drupal_cron_run()` | `CronService::run()` | Aligned |
| Cron key | `/cron/{key}` endpoint | Aligned |
| `DrupalQueue` | `Queue` trait + `RedisQueue` | Aligned |
| `hook_queue_info` | `tap_queue_info` | Aligned |
| Queue worker | `tap_queue_worker` | Aligned |
| Poor man's cron (on page request) | Not implemented (external scheduler) | Missing |
| Distributed cron locking | Redis SET NX EX with heartbeat | Aligned |

**Key files:**
- `crates/kernel/src/cron/mod.rs` -- CronService, distributed locking
- `crates/kernel/src/cron/tasks.rs` -- task implementations
- `crates/kernel/src/cron/queue.rs` -- Queue trait, RedisQueue

---

## 15. Cache System

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| `cache_set` / `cache_get` | `CacheLayer::get/set` | Aligned |
| Cache bins (`cache`, `cache_menu`, `cache_filter`) | Single namespace with tag-based organization | Aligned (intentional divergence) |
| `cache_clear_all()` | Tag-based invalidation | Aligned |
| Cache expiration | TTL-based (L1: 60s, L2: 300s) | Aligned |
| Database-backed cache | Two-tier: Moka (in-process) + Redis | Aligned (intentional divergence) |
| Cache tags | Tag-based invalidation via Lua + Redis SETs | Aligned |

**Key files:**
- `crates/kernel/src/cache/mod.rs` -- CacheLayer, two-tier L1/L2, tag invalidation

---

## 16. URL Aliases / Path System

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| `url_alias` table | `url_alias` table | Aligned |
| `drupal_get_path_alias()` | path_alias middleware rewrite | Aligned |
| `drupal_get_normal_path()` | Internal path from alias | Aligned |
| Stage-aware aliases | `stage_id` column | Aligned |
| Language-aware aliases | `language` column | Aligned |
| Pathauto (auto-generation) | Not implemented | Missing |
| Bulk alias management | Not implemented | Missing |

**Key files:**
- `crates/kernel/src/middleware/path_alias.rs` -- resolve_path_alias middleware

---

## 17. Batch API

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| `batch_set()` | `BatchService` | Aligned |
| Batch progress | `BatchProgress` (processed/total) | Aligned |
| Browser-driven execution | Server-side async execution | Aligned (intentional divergence) |
| Progress bar UI | Not implemented | Missing |
| Batch states (pending/running/complete/failed) | All states supported | Aligned |

**Key files:**
- `crates/kernel/src/batch/service.rs` -- BatchService, BatchOperation, BatchProgress

---

## 18. Comments

| D6 Concept | Trovato Equivalent | Status |
|---|---|---|
| `comment` table | Comment model + plugin | Aligned |
| Comment threading | Threaded display | Aligned |
| Comment moderation | Admin moderation UI | Aligned |
| Per-content-type comment settings | Plugin configuration | Aligned |
| `hook_comment` | Plugin-handled | Aligned |
| Comment subscriptions/notifications | Not implemented | Missing |

**Key files:**
- `plugins/comments/` -- Comments WASM plugin (permissions, menu)
- `crates/kernel/src/models/comment.rs` -- Comment model with `parent_id`, `depth`, recursive CTE for threaded listing
- `crates/kernel/src/routes/comment.rs` -- comment CRUD routes

---

## 19. Modern Additions (Exceed D6)

These features have no D6 equivalent and represent architectural improvements:

| Feature | Trovato Implementation | D6 Equivalent |
|---|---|---|
| Content Staging (Stages) | `StageService` with stage_association, publish phases | None (D6 had only published/unpublished) |
| OAuth2 Provider | OAuth2 plugin + kernel service/routes/middleware | None (required contrib) |
| Webhooks | Webhook plugin + delivery queue with retry | None (required contrib) |
| Scheduled Publishing | Plugin + cron handler | None (required contrib) |
| Content Locking | Plugin + kernel lock service | None (required contrib) |
| Audit Logging | Plugin + kernel audit service | watchdog/dblog (basic) |
| Redirects | Plugin + kernel redirect service/cache | None (required contrib) |
| Config Export/Import | YAML export/import with CLI | None (Features module was contrib) |
| Config Translation | Plugin + kernel translated config wrapper | None (i18n was contrib) |
| Content Translation | Plugin + kernel translation service | None (i18n was contrib) |
| Locale (UI Strings) | Plugin + kernel locale service + PO parser | locale module (core, but limited) |
| Image Styles | Plugin + kernel image processing | imagecache (contrib) |
| Rate Limiting | Tower middleware | None |
| Prometheus Metrics | `/metrics` endpoint | None |
| Profiling (Gander) | Request profiling middleware | Devel module (contrib) |
