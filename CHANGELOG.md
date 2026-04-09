# Changelog

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
