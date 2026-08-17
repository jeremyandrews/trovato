# Changelog

## Unreleased

- `config import` no longer reports success over files it could not apply, and
  the tutorial's own config set now imports clean. Two halves of one defect.

  **The reporting half.** Import walked the config directory applying whatever
  parsed. A file that failed — malformed YAML or content that no longer matched
  its entity's schema — became one line in a warning list, did not stop the run,
  and did not change the exit code. `trovato config import <dir>` on a set with
  one typo printed "Imported N config entities" and exited 0. Roles and stages
  have no admin form (`KNOWN-ISSUES.md`), so for those two types import is the
  only management path and a skipped file was an entity that never arrived with
  nothing saying why.

  Import now validates before it applies. Every file is read, parsed and schema
  checked, and every reference it makes (a tag's category, a search field
  config's bundle, a tag's parents) is resolved against the import set and then
  the database — all before the first write. If anything fails, the run returns
  `ConfigImportFailed` naming every offending file with its reason, the CLI exits
  non-zero, and **nothing is written**: not even the valid files sitting next to
  the bad one, so a config set is atomic with respect to bad input. `--dry-run`
  runs exactly that validation and skips only the writes, which makes it a real
  preflight instead of a report of what would have been attempted. A failure in
  the save pass is also an error rather than a warning; those writes are not in
  one transaction (`ConfigStorage` is a trait over backends), so earlier writes
  stand and the fix is to re-run, but the exit code no longer says success.

  The failure messages also carry their cause now. `{e}` on an `anyhow::Error`
  prints only the outermost context, so every one of the tutorial's 18 failures
  read "invalid tile YAML" or "invalid stage YAML" with no hint of what was
  wrong; `{e:#}` appends the chain, so the same failure now names the missing
  field.

  What stayed a warning, deliberately: a file the config set does not claim (an
  unrecognized filename prefix, a symlink, an oversized file), a filename whose
  ID disagrees with its content, and a duplicate entity ID. These are advisory
  observations about the directory rather than a recognized config file that
  cannot be applied, and promoting them would hard-fail directories that are
  working today.

- The tutorial's config set no longer drifts from the config schemas. 18 of its
  76 files did not parse — every stage, role, tile and menu link — so
  `config import docs/tutorial/config` applied 58 and skipped the rest while
  reporting success. Each was missing required fields: `id` on all of them,
  `machine_name` on stages, `stage_id` and the timestamps on tiles and menu
  links. The files were repaired against the current schemas; no schema was
  loosened to accept them.

  They were also renamed. Import names an entity's file `{entity_type}.{id}.yml`
  where the ID is the entity's own identifier, and roles, stages, tiles and menu
  links are keyed by UUID, so `stage.incoming.yml` disagreed with its content and
  would have warned on every import. They now use the UUID that `config export`
  would write, drawn from the `0193a5a0-` family the stage seeds already use, so
  the directory is what an export produces and re-importing it round-trips.
  `docs/tutorial/config/README.md` is a new index mapping each UUID back to its
  machine name, and the tutorial and recipe text that referenced the old names
  was updated — including two recipe steps that told the reader roles and stages
  were "not importable" and had them create both by hand, which after the repair
  would collide with what the import creates.

- `DirectConfigStorage::save_stage` now honors the UUID a stage's config file
  declares, and its update path works at all. Two bugs found by making the
  tutorial's stage files import:

  The create path called `Stage::create`, which generates its own `Uuid::now_v7()`
  and ignored the `id` in the file. So a stage landed under a UUID nobody
  declared, export would not round-trip it, and a **second import failed**: the
  lookup by declared id still missed, so it tried to create the stage again and
  collided on `stage_config.machine_name`'s unique constraint. `create` now
  delegates to a new `Stage::create_with_id`, and config import passes the file's
  id.

  The update path ran `UPDATE stage_config ... WHERE stage_id = $4`, and that
  column is named `tag_id` — it has been `tag_id` since
  `20260225000002_create_stage_config.sql` and was never renamed. Every update of
  an existing stage therefore failed with "column stage_id does not exist",
  which importing the well-known Live stage hits immediately. `delete_stage` had
  the same wrong column. Fixed both, and the update now also writes
  `category_tag`'s label, description and weight inside a transaction with the
  `stage_config` update; it previously touched only `stage_config`, so
  re-importing a relabelled or reweighted stage silently did nothing.

- `config export` no longer fails on a database that contains a tag.
  `DirectConfigStorage::fetch_all_tags` selected seven of `Tag`'s eight columns,
  omitting `slug`, so `query_as::<_, Tag>` failed at row decode with "no column
  found for name: slug" and the command exited non-zero. The column has existed
  since `20260307000001_add_category_tag_slug.sql`; only this one query never
  asked for it, and it is the only `FROM category_tag` select in that file, so
  nothing compensated.

  The blast radius was every real site: stages are rows in the `stages` category,
  so a database has a tag as soon as it has stages, and `config export` was
  therefore broken for anyone with anything to export. `config import` was
  unaffected, because it never reads tags back, which is why an application could
  install from files while being unable to write them back out.

  Existing tag coverage missed it for a specific reason. `list("tag", filter)`
  routes a `category_id` filter to `Tag::list_by_category`, which does select
  `slug`; only the unfiltered path that export uses was broken, and the tag test
  listed by category. The regression test exports a database holding a tag with a
  slug set and asserts on the exported file's contents, so neither dropping the
  column from the struct nor restoring the old select can pass it.

- The admin record view route now honors the key column a record type actually
  declares. `[[record_types]]` lets a plugin name its own `id_column`, and every
  other reader trusts it — the list route projects `{id_column}::text`, gather
  resolves the logical `id` to it — but the view handler extracted the path
  segment as `Path<(String, uuid::Uuid)>`. Axum parses that before the handler
  body runs, so a record type keyed by a bigint (or any other scalar) listed
  correctly and returned **400 on every row link**, with the guard and the
  registry lookup never reached. The route was hard-coding the id type in the one
  place that did not need to assume it, one line above the query it built from
  `def.id_column`.

  The id is extracted as a string and compared as text, `WHERE {id_col}::text =
  $1`, with the segment bound as a parameter — as injection-safe as before, since
  the column name is still the registry-validated identifier and the value is
  still bound. One path serves a uuid key, a bigint key and any other scalar key,
  with no per-type branching and no registry addition. A uuid-shaped segment is
  normalized to the canonical lowercase form Postgres renders `uuid::text` as, so
  the uppercase, braced and unhyphenated spellings the uuid extractor accepted
  still open their row. A miss renders not-found rather than erroring, including
  for a segment that is not a number at all on a bigint-keyed type, and
  `require_admin` still gates the route.

  The reference plugin `trovato_record_ref` grew a second record type,
  `legacy_record` over a BIGINT-keyed table, so the non-uuid case is exercised
  end-to-end through the real router and the real registry rather than asserted
  in the abstract.

- `Config::from_env` is now the only place in the process that reads the
  environment. Twelve settings were read lazily and repeatedly at the point of
  use instead: the static search path on every served file, the CSP headers on
  every response, the cron key on every call to `/cron/{key}`, the tenant
  strategy and the slow-request threshold on every request, the security-audit
  retention window on every prune, and the gather over-fetch bounds through
  `LazyLock` statics that froze on first touch. Reading configuration at the
  point of use has three costs, and this pays off all of them: the work is
  repeated (a `HeaderValue` was rebuilt and re-parsed per response, a search path
  re-split per file), the value cannot be steered by a caller at all, and the
  only way for a test to reach it was to mutate a process-global.

  Values request handling needs travel on a new `RuntimeConfig`, carried on
  `AppState` and reachable as `state.runtime()`. It is kept separate from
  `Config` deliberately: `Config` holds `database_url`, `smtp_password` and
  `jwt_secret`, and `AppState` is handed to every request handler. Startup-only
  settings — `templates_dirs`, `trusted_proxies`, `jwt_secret`,
  `shutdown_timeout` — became `Config` fields consumed where they already were.
  Each group of settings resolves through a `from_lookup` constructor owned by
  the module it configures (`SecurityHeaders`, `TenantResolution`,
  `GatherAccessConfig`), so every setting name and documented default is
  assertable from an explicit map.

  Two fixes fell out of the consolidation. An unusable `CSP_REPORT_URI` — one
  with a newline or a control character — used to leave the response with **no
  CSP at all**, because the `HeaderValue::from_str` failure skipped the insert
  entirely; it now logs and serves the policy without the report endpoint. And
  `QUERY_SLOW_THRESHOLD_MS` multiplied by five without a guard, so a large
  configured threshold could overflow; the comparison saturates.

  Behaviour is otherwise unchanged, including every default and the precedence
  of an explicitly set variable. `PATH`, the per-provider AI key variable, and
  `site_config`'s `env:` secret references still read the environment on use, and
  correctly so: in each the *variable name* is runtime data, not configuration.

- Tests no longer mutate the process environment without a guard. Seven places
  called `std::env::set_var` / `remove_var` from code running on a live libtest
  thread pool: the environment is process-global, `cargo test` runs a binary's
  tests in parallel, and `setenv` is not thread-safe against the `getenv` that
  dependency C code performs for timezone lookups and name resolution. None of
  the mutating tests restored what they found unless every assert passed, so one
  failure leaked state into every test that ran afterwards, and the SAFETY
  comments justifying the `unsafe` blocks asserted a "before any threads are
  spawned" guarantee that libtest does not provide.

  The fix is mostly to remove the need. `PluginConfig::from_env`,
  `audit::retention_days_from_env` and `config::split_search_path` are now
  one-line edges over `from_lookup`, `retention_days_from` and
  `split_search_path_value`, which take their input as a parameter; their tests
  drive those cores with explicit values and touch nothing global, and they
  cover the defaults and the unparseable-value fallbacks that the old
  environment-reading tests could not state (asserting a default by *assuming*
  a variable was unset failed spuriously on any machine that exported it). The
  integration fixtures set `Config` fields — `plugins_dirs`,
  `database_max_connections` — instead of the variables those fields are loaded
  from. What legitimately remains goes through one mechanism for the whole
  workspace, `trovato_test_utils::env`: a single lock serializing every
  mutation, an `EnvGuard` that restores what it found when it drops (including
  while a failing assert unwinds), and `load_dotenv` so that `dotenvy`'s own
  `set_var` calls take the same lock. Every SAFETY comment now states only what
  is actually guaranteed, and names the part that no test-side lock can close.

  No runtime behaviour changes. The variables still read lazily and repeatedly
  deep inside runtime call paths — the cron key, CSP headers, tenant
  resolution, the query profiler threshold, `TEMPLATES_DIR`, `STATIC_DIR`,
  `TRUSTED_PROXIES` — are why a fixture has to reach for the environment at
  all; consolidating them into the startup config is the follow-up.

- `STATIC_DIR` is a search path, like `PLUGINS_DIR` and `TEMPLATES_DIR`.
  0.99.0 generalized two of the three asset roots and left the third a single
  directory, so an application could overlay its templates but not the CSS
  those templates reference: shipping a stylesheet meant writing into the one
  `./static`, which is the kernel tree the overlay exists to leave alone.
  Several directories separated by the platform separator are now read in
  order, a later one overrides an asset of the same path, and the asset
  manifest hashes the file that is actually served. A single-directory value
  is unchanged in behaviour. The generated Pagefind index needs one
  destination and is written to the first directory.

  ```
  STATIC_DIR=./static:/path/to/app/static trovato serve
  ```

- The site front page can be any path the site serves. `site_front_page` was
  parsed as `/item/{uuid}` and nothing else, so every other value — a gather
  alias, a plugin route, an aliased path — silently fell back to the promoted
  listing, and the home page was the one route on the site that could not be
  any route. An item path still renders inline at `/`; any other path
  redirects (307) to itself, carrying the query string, so the handler that
  owns the route serves it and the front page needs no knowledge of route
  types. The path must be local (absolute, no scheme, no host); the admin form
  rejects a non-local value with the same rule that serves it.
- The front page listed only promoted items that happened to be among the ten
  most recent published items: it fetched a fixed page of published items and
  filtered it in memory. Promotion now decides the query
  (`Item::list_promoted`, with paging), so a promoted item appears however many
  newer published items precede it.
- An item front page was access-checked against a hard-coded permission list
  rather than the viewer's real permissions, so anonymous visitors were handed
  none at all and a configured item front page fell through to the promoted
  listing on a default install (whose anonymous role does have "access
  content"). The front page now builds the same user context as the item
  route.
- Navigation hid every permission-gated menu item from everyone. The menu was
  built as `root_menus().filter(|m| m.permission.is_empty())`, which reads as a
  permission check and is not one: it kept only the entries needing no
  permission, so an entry declaring one was dropped for every viewer, including
  a viewer who held it and an admin. A plugin could not get a gated navigation
  entry into the menu at all — Argus's `/stories` and `/articles`, both gated on
  `access content`, were unreachable from the navigation for everyone.
  `MenuRegistry::root_menus_for` now filters per viewer: an entry appears when
  it requires no permission, when the viewer holds the one it requires, or when
  the viewer is an admin, matching `require_permission`.
- Admin contexts are loaded rather than fabricated. `admin_user_context`
  returned `authenticated(user.id, vec!["administer site"])`, dropping the
  admin's real permissions across eleven admin handlers, and the admin AJAX
  callback hard-coded the same list. Both worked only because `is_admin()`
  short-circuits every permission check — the same fabricate-instead-of-load
  shape that broke the item front page, one short-circuit from the same
  failure. Every request-scoped context now comes from one loader:
  `PermissionService::user_context` assembles it, `routes::helpers`
  `get_user_context` (from a session) and `user_context_for` (from an
  already-loaded `User`) are the two sanctioned ways to reach it, and the
  hand-rolled copies in `routes::comment` and the MCP server were folded into
  it.
- Self-service updates pass the owner's real permissions as the acting
  principal. Profile update, password change, email-change verification and
  password reset built `authenticated(id, vec![])`. Those service methods
  authorize nothing — the route gates on identity or a verified token — but the
  context is dispatched to plugins as the `tap_user_update` principal, so an
  empty list told every listener that a permission-less user acted. Documented
  on `UserService::update` and `update_password`: `acting_user` is the tap
  principal, not an authorization subject.
- A failed permission load no longer degrades a request silently. Both loaders
  ended in `unwrap_or_default()`, so a transient database error produced an
  empty permission set — safe, but indistinguishable from a permission model
  that is working, which is exactly how the front-page bug presented. The
  policy is now explicit per caller: paths that can propagate fail closed
  (`PermissionService::user_context`, the MCP server), and the web loader
  degrades to the deny-all set while logging at ERROR and incrementing
  `trovato_permission_load_failures_total`, which should alert on any non-zero
  value.
- wasmtime 43 → 47.0.3. Clears every outstanding wasmtime and cranelift
  advisory (RUSTSEC-2026-0085 through -0096, -0114, -0222); 14 suppressions
  removed from `.cargo/audit.toml`. No source change was required and the
  plugin ABI is unaffected: a plugin binary built against the pre-freeze SDK
  still loads and runs.

## v0.99.0 — 2026-08-16

First public release. Pre-1.0: the plugin contract is frozen, the CMS is not
finished. See `KNOWN-ISSUES.md` and `ROADMAP.md`.

### Versioning

- One version for the whole project. Every crate inherits `version.workspace`;
  every plugin manifest carries the project version; the plugin API is that
  version without the patch component (`0.99.0` → `(0, 99)`,
  `api_version = "0.99"`). Replaces four independent version tracks. See
  `docs/design/Versioning.md` and `docs/design/version-map.md`.
- Added `trovato --version`.
- OpenAPI `info.version` now reads the build version instead of a literal.

### Licensing

- Dual licensed MIT OR Apache-2.0. Replaces the `GPL-2.0-or-later` declaration
  used during private development; nothing was published under it.

### External applications

- `PLUGINS_DIR` and `TEMPLATES_DIR` are search paths: several directories
  separated by the platform separator, read in order, later wins a name
  collision. A single-directory value is unchanged in behaviour.
- Templates from all roots load into one Tera instance, so an application
  template can extend a kernel template.
- Plugin name collisions across the search path are logged.
- Additive; no plugin ABI change.

An application now installs from its own repository, adding no file to the
kernel tree:

```
PLUGINS_DIR=./plugins:/path/to/app/plugins \
TEMPLATES_DIR=./templates:/path/to/app/templates \
trovato serve
trovato config import /path/to/app/config
```

---

Entries below predate the first public release and use private development
version numbers. No release was published under them.

## v0.2.0-beta.2 — 2026-04-09

### Infrastructure Hardening

- **Structured Error Handling**: 12-variant `AppError` with JSON `ErrorResponse` (machine-readable codes, request IDs, per-field validation details). PostgreSQL errors classified by code (23505 → 409 Conflict, 23502 → 422). All 160+ route handlers migrated from ad-hoc tuples.
- **Circuit Breakers**: AI provider (3 failures/60s recovery), email SMTP (5/30s), S3 storage (3/30s). States visible in `/health` and `/metrics` (Prometheus gauges).
- **Graceful Shutdown**: SIGINT/SIGTERM handling with configurable drain timeout (`SHUTDOWN_TIMEOUT_SECS`). Background tasks use `CancellationToken` for coordinated exit.
- **DB Pool Monitoring**: 4 Prometheus gauges (size/idle/active/max). `/health` includes pool utilization. Background task warns at 80%+ utilization.
- **Concurrent Plugin Loading**: WASM compilation via `tokio::task::spawn_blocking` per plugin. Failed plugins logged and stored for admin visibility instead of aborting startup. Mtime tracking for reload optimization.
- **Async Plugin Discovery**: Directory scanning uses `tokio::fs`. All callers updated.

### CMS Product Features

- **Admin Site Configuration UI**: `/admin/config/site` with site name, slogan, email, front page, items per page, registration mode, SMTP settings, and notification preferences. Test email button for SMTP verification.
- **Email Template System**: 4 template pairs (HTML + plain text) for registration verification, password reset, comment notification, and admin new-user alerts. Multipart sending via `send_templated()`.
- **Email Notifications Wired**: Comment creation notifies content author (background task, skips self-notifications). User registration notifies admin when enabled in site config.
- **Media Browser**: `/admin/media` grid view with type/search filters, pagination. `/api/v1/media/browse` JSON API. JavaScript media picker modal with browse/upload tabs, drag-and-drop, integrated into content edit form file fields.
- **SEO Plugin** (`trovato_seo`): Meta description, Open Graph tags, JSON-LD structured data (Article, Event, FAQPage schema types with speakable property), sitemap.xml with URL alias resolution, robots.txt with AI crawler management (GPTBot, ChatGPT-User, ClaudeBot, Google-Extended, Bytespider, CCBot, PerplexityBot, Amazonbot).

### Versioning & Release

- **Plugin API Versioning**: `api_version` field in all 25 plugin manifests. `KERNEL_API_VERSION` constant. Compatibility check at install and enable time (major must match, minor must be <=).
- **Workspace Version**: All core crates inherit version from `[workspace.package]` (single-line bumps).
- **Nightly Auto-Increment**: Docker versions increment per commit (counts commits since tag).
- **Versioning Documentation**: `docs/design/Versioning.md` with full strategy.

### Search & AI Improvements

- **Rich Search Results**: Pagefind index includes description, location, event dates, content type metadata. Friendly URL aliases. Type badges, dot-separated metadata chips in result cards.
- **Chatbot RAG Enrichment**: Loads actual items from DB after search — context includes all JSONB field values (dates, locations, descriptions, URLs) instead of just title + snippet.
- **Chat Formatting**: `formatChat()` renders markdown (bold, numbered lists, bullet points) in chat widget.
- **Auto-Search**: Scolta.js reads `?q=` URL parameter and searches on page load.

### Rate Limiting

- **Per-IP Rate Limiting**: Middleware wired into request chain (before session resolution).
- **Per-User Rate Limiting**: Second middleware layer after authentication, keyed on user ID.
- **Bug Fix**: `/user/register` now correctly maps to the `register` category (3/hr) instead of `login` (5/min).

### Test Coverage

- **924 total tests** (up from 754 in beta.1): 829 kernel, 14 SEO plugin, 81 MCP server.
- New coverage: middleware (rate limit categories, client ID extraction, security headers), error system, circuit breakers, route helpers, email templates, content filters, MCP tools/resources/server.

### Tutorial & Documentation

- Fixed plugin names in tutorial Parts 5-7 (block_editor → trovato_block_editor, .wasm path → machine name in install commands).
- Pre-commit hook (`.githooks/pre-commit`) runs fmt + clippy automatically.
- `scripts/pre-commit-check.sh` with `--quick`, standard, and `--full` modes.

---

## v0.2.0-beta.1 — 2026-04-04

First public beta release.

### Core CMS

- **Content Types**: Dynamic content types with custom fields (Text, TextLong, Integer, Float, Boolean, Date, Email, File, RecordReference), JSONB field storage, revision history
- **Gather Query Engine**: 18 filter operators (Equals, Contains, HasTag, HasTagOrDescendants, FullTextSearch, SemanticSimilarity, etc.), configurable pagination, exposed filters, NULLS FIRST/LAST ordering
- **Categories**: DAG hierarchy with multiple parents per tag, recursive ancestor/descendant queries, slug-based routing
- **Full-Text Search**: PostgreSQL tsvector with configurable field weights, GIN indexes, integrated as gather filter
- **URL Aliases**: Clean URLs with middleware-based resolution, automatic pathauto generation from title patterns
- **Redirects**: URL redirect management with automatic alias-change tracking and loop detection
- **Content Staging**: Stage hierarchy with parent/child chains, upstream publishing, content overlay inheritance
- **Block Editor**: Editor.js with 8 block types (paragraph, heading, image, list, quote, code, delimiter, embed), server-side rendering with ammonia sanitization, syntect syntax highlighting
- **Forms**: Declarative Form API with validation, AJAX multi-step support, CSRF protection
- **Config Import/Export**: YAML-based with 13 entity types (item_type, item, role, stage, tile, menu_link, category, tag, gather_query, url_alias, language, variable, search_field_config)
- **File Uploads**: Magic byte validation, filename sanitization, MIME type checking, image style derivatives

### Plugin System

- **WASM Sandboxing**: 25 plugins compiled to WebAssembly, running in per-request Wasmtime sandboxes with pooled allocation (~5us instantiation)
- **Tap System**: 40+ named extension points for content, forms, access control, menus, permissions, cron, search, AI, and chat
- **Host Functions**: All fully implemented — database (parameterized queries, DDL guards), item CRUD, cache (Moka L1 + Redis L2), variables (persistent key-value via site_config), AI requests, HTTP, crypto (SHA-256, HMAC-SHA256, random bytes, constant-time comparison), user context, logging, queues
- **Plugin Namespace**: All core plugins use `trovato_` prefix convention. Standalone projects (argus, netgrasp, goose) and ritrovo_* plugins retain their own namespaces
- **Plugin CLI**: `trovato plugin list|install|migrate|enable|disable|new`

### Standard Plugins (25)

**Content Types**: trovato_blog, trovato_media, argus (7 types), netgrasp (6 types), goose (5 types)

**Features**: trovato_categories, trovato_comments, trovato_audit_log, trovato_content_locking, trovato_scheduled_publishing, trovato_webhooks, trovato_image_styles, trovato_oauth2, trovato_redirects, trovato_block_editor

**i18n**: trovato_locale, trovato_content_translation, trovato_config_translation

**AI**: trovato_ai (field rules, form assist, chat actions), trovato_search

**Reference App**: ritrovo_importer, ritrovo_cfp, ritrovo_access, ritrovo_notify, ritrovo_translate

### AI Integration

- **Provider Registry**: OpenAI-compatible and Anthropic protocols, secure key store (env vars, never in DB or WASM)
- **`ai_request()` Host Function**: Single function for Chat, Embedding, ImageGeneration, SpeechToText, TextToSpeech, Moderation
- **Token Budgets**: Per-user, per-role tracking with configurable daily/weekly/monthly periods
- **Field Rules**: Automatic content enrichment on save via `tap_item_presave` (fill_if_empty, always_update behaviors)
- **Form AI Assist**: Inline rewrite/expand/shorten/translate/tone buttons on text fields via `tap_form_alter`
- **Chatbot**: SSE streaming chat with RAG context from search, session history, configurable system prompt
- **MCP Server**: Content CRUD, search, Gather, categories, content_types tools + resources for external AI tools
- **VectorStore**: Trait with PgVectorStore implementation, SemanticSimilarity gather operator, graceful degradation without pgvector

### Security

- **Authentication**: Argon2id (RFC 9106 params), Redis sessions, account lockout, session fixation protection
- **OAuth2**: Authorization code (PKCE), client credentials, refresh token grants with JWT
- **Security Headers**: CSP, X-Frame-Options: DENY, HSTS, X-Content-Type-Options, Referrer-Policy, Permissions-Policy
- **Crypto Host Functions**: SHA-256, HMAC-SHA256, secure random bytes, constant-time comparison for WASM plugins
- **SSRF Prevention**: DNS rebinding mitigation, private IP blocking, port restrictions
- **File Upload**: Magic byte validation, filename sanitization, MIME allowlist, executable rejection

### Accessibility

- Skip links, main landmark, focus-visible CSS, visually-hidden utility
- 8 ARIA helpers on ElementBuilder (SDK): aria_label, aria_describedby, aria_hidden, aria_current, aria_live, role, aria_expanded, aria_controls
- Form elements with aria-describedby, aria-invalid, role="alert" on error summaries
- Flash messages with role="status" and aria-live auto-announcement
- Admin tab navigation with full WAI-ARIA (role="tablist/tab", aria-selected, arrow key navigation)
- Correct heading hierarchy (h1 > h2) across all templates

### Internationalization

- Language negotiation: URL prefix, Accept-Language, session, site default
- RTL support: 15 language codes, text_direction_for_language(), dir attribute on html
- Locale-aware date formatting: 14 locale patterns (en, de, fr, es, it, ja, zh, ko, ar, he, pt, nl, ru, pl)
- Interface translation: Gettext .po import, in-memory cache, Tera `t()` function
- Content translation: Field-level overlay with language fallback
- Config translation: Translatable configuration entities

### Infrastructure

- **Two-Tier Cache**: Moka L1 (in-process) + Redis L2 with tag-based invalidation
- **Multi-Tenancy**: Tenant schema, middleware, tenant_id on all content tables, zero-overhead single-tenant default
- **API**: Versioned router (/api/v1/), X-API-Version header, paginated ListEnvelope, route metadata registry
- **Docker**: Nightly images published to ghcr.io on every push to main. Three workflows: native dev, dev container, pre-built runtime
- **CI**: 8-job pipeline — Security Audit, Build, Clippy, Doc Check, Test, Format, Terminology Check, Coverage

### Known Limitations

- **Search**: Full-text search works; semantic search (query expansion via AI, faceted search) not yet implemented
- **Performance**: No formal benchmark suite or profiler integration
- **Nightly Images**: amd64 only; arm64 images published with release tags. ARM users can build locally
- **RecordReference Autocomplete**: Works but searches published items only (no draft item search)
