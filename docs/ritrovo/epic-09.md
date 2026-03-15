# Epic 9: Going Global

**Tutorial Part:** 7
**Trovato Phase Dependency:** Phase 4 (i18n, REST API), Phase 5 (Translation Workflow)
**BMAD Epic:** 38
**Status:** Not started

---

## Narrative

*Ritrovo speaks English. One language, one URL scheme, one audience. Part 7 breaks every one of those walls. The site goes bilingual (English + Italian), gets a translation workflow powered by the fifth and final Ritrovo plugin, and opens a JSON REST API so external tools can consume conference data programmatically.*

The reader learns three big concepts. First, how Trovato handles multilingual content: JSONB parallel field sets for translations, language-prefixed URL aliases, a language switcher in the header, and locale-aware template rendering. Second, how the `ritrovo_translate` plugin detects non-English content, flags it for translation, and provides an editorial workflow for translators. Third, how the Gather engine that powers HTML pages also powers a thin JSON API layer -- the same queries, the same access control, the same pagination, just serialized differently.

By the end, Ritrovo serves two languages, translates content through an editorial queue, and exposes a documented REST API with authentication and rate limiting.

---

## Tutorial Steps

### Step 1: i18n Architecture

Configure Trovato for bilingual operation (English default + Italian). Cover the content translation model, locale routing, and UI string translation.

**What to cover:**

- Bilingual configuration: English (default) + Italian
- Content translation model: JSONB parallel field sets -- translatable fields (`name`, `description`, `city`) stored as `{"en": "value", "it": "valore"}` within the field value
- Which fields are translatable vs. language-neutral (dates, URLs, booleans are language-neutral)
- Locale files for UI strings (`.po` or JSON format): button labels, form labels, error messages, navigation text
- Language detection from URL prefix (`/it/conferences` vs. `/conferences`), user preference cookie, or `Accept-Language` header
- Language switcher in the site header: "English / Italiano" links

### Step 2: Translated URL Aliases

Configure language-specific URL aliases so each language has clean, localized URLs.

**What to cover:**

- URL alias patterns per language: `/conferences/rustconf-2026` (EN), `/it/conferenze/rustconf-2026` (IT)
- Language prefix routing: `/it/` prefix routes to Italian content; no prefix defaults to English
- Pathauto patterns per language: `conferences/[title]` (EN), `it/conferenze/[title]` (IT)
- `hreflang` tag generation: `<link rel="alternate" hreflang="it" href="/it/conferenze/rustconf-2026">` for SEO
- Redirect handling: visiting the English URL with `?lang=it` redirects to the Italian alias
- How URL alias resolution interacts with language prefix routing in the middleware stack

### Step 3: The `ritrovo_translate` Plugin

Build the translation workflow plugin: language detection, translation flagging, and an editorial queue for translators.

**What to cover:**

- `tap_item_insert` -- On new conference creation, detects language from text fields (title, description). If non-English, flags for translation by setting a `translation_status` metadata field
- `tap_item_view` -- Adds language indicator badge (flag icon or language code). Shows "View in English / Vedi in italiano" switcher on detail pages
- `tap_cron` -- Processes the translation queue. For the tutorial, translations are seeded statically; the cron handler checks for items flagged as needing translation
- `tap_form_alter` -- Adds a language selector dropdown to the conference edit form
- Translation editorial workflow: translators see a queue of items needing translation at `/admin/content/translations`. Side-by-side form shows source language and target language fields
- SDK features demonstrated: i18n integration, `tap_form_alter` for form modification, `tap_item_view` for language badge injection, cron-based queue processing
- Stretch goal: optional LLM-powered auto-translation via `ai_request()` from Epic 3 (disabled by default)

### Step 4: Seeding Italian Content

Seed the bilingual demo: ~20 Italian conferences with hand-written English translations, demonstrating the full translation workflow.

**What to cover:**

- Seed data: ~20 Italian tech conferences with Italian names and descriptions
- English translations provided for each (hand-written, not machine-translated, for tutorial quality)
- Translation status: some items marked as "translated," others as "needs translation" to show the queue in action
- Language-aware Gather queries: filter by language, show results in the user's preferred language
- Template rendering: Tera templates check the active language and render the appropriate field translation
- Verify: `/conferences` shows English content, `/it/conferenze` shows Italian content

### Step 5: REST API

Open a JSON API layer on top of the Gather engine. Cover endpoints, authentication, rate limiting, and stage awareness.

**What to cover:**

- API design: thin JSON serializer on top of the same Gather engine that powers HTML pages
- Endpoints:
  - `GET /api/v1/conferences` -- list upcoming conferences (supports `?topic=`, `?country=`, `?online=`, `?page=`, `?per_page=`)
  - `GET /api/v1/conferences/{id}` -- single conference with all fields, speakers, comments
  - `GET /api/v1/topics` -- full topic category hierarchy
  - `GET /api/v1/topics/{id}/conferences` -- conferences for a topic (includes descendants)
  - `GET /api/v1/search?q=` -- full-text search
  - `GET /api/v1/speakers` -- list speakers
  - `GET /api/v1/speakers/{id}` -- single speaker with linked conferences
  - `POST /api/v1/conferences` -- create conference (editor+ required)
  - `PATCH /api/v1/conferences/{id}` -- update conference (editor+ required)
  - `POST /api/v1/conferences/{id}/subscribe` -- subscribe (authenticated)
  - `DELETE /api/v1/conferences/{id}/subscribe` -- unsubscribe (authenticated)
- Authentication: API key in `Authorization: Bearer {key}` header, keys managed in user profile
- Rate limiting: Tower middleware, per-role. Anonymous: 60 req/min. Authenticated: 300 req/min
- Stage awareness: API returns Live content by default. `?stage=curated` available to editors+
- Language awareness: `?lang=it` returns Italian translations when available
- Error responses: consistent JSON format with `error` and `status` fields

### Step 6: API Documentation & Testing

Document the API and verify all endpoints work correctly.

**What to cover:**

- API documentation: endpoint reference with request/response examples
- Testing each endpoint with curl: list, single, filtered, authenticated, rate-limited
- Error cases: 404 for missing items, 403 for unauthorized stage access, 429 for rate limit exceeded
- Pagination: `page`, `per_page` parameters, response includes `total`, `page`, `per_page` metadata
- Content negotiation: `Accept: application/json` header

---

## BMAD Stories

### Story 38.1: Multilingual Content Model

**Status:** Not started

**As a** site builder,
**I want** content to support multiple languages via JSONB parallel field sets,
**So that** conferences can be displayed in English or Italian.

**Acceptance criteria:**

- Bilingual configuration: English (default) + Italian
- Translatable fields store values as `{"en": "...", "it": "..."}` within the JSONB field value
- Non-translatable fields (dates, booleans, URLs) stored as single values regardless of language
- Item Type definition distinguishes translatable vs. non-translatable fields
- Tera templates check the active language and render the appropriate translation
- Fallback behavior: if a translation is missing, display the default language content
- Language stored per-item as `language` field (primary language of original content)

### Story 38.2: Language Routing & URL Aliases

**Status:** Not started

**As a** site visitor,
**I want** localized URLs and a language switcher,
**So that** I can browse the site in my preferred language.

**Acceptance criteria:**

- Language prefix routing: `/it/` prefix activates Italian locale; no prefix defaults to English
- Pathauto generates language-specific aliases: `conferences/rustconf-2026` (EN), `it/conferenze/rustconf-2026` (IT)
- Language switcher in site header: "English / Italiano" links that switch to the equivalent page in the other language
- `hreflang` tags generated in page `<head>` for SEO
- Language preference stored in cookie; subsequent visits default to preferred language
- `Accept-Language` header used for first-visit language detection
- URL alias resolution handles language prefix correctly in the middleware stack
- Locale files loaded for UI strings (button labels, navigation, error messages)

### Story 38.3: `ritrovo_translate` Plugin -- Translation Workflow

**Status:** Not started

**As a** site translator,
**I want** a workflow for translating conference content,
**So that** non-English conferences are accessible to English-speaking users.

**Acceptance criteria:**

- WASM plugin `ritrovo_translate` compiled and installable
- `tap_item_insert`: detects language from text fields on new items, sets `translation_status` metadata
- `tap_item_view`: adds language indicator badge and "View in English / Vedi in italiano" switcher
- `tap_cron`: processes translation queue -- checks for items needing translation
- `tap_form_alter`: adds language selector dropdown to conference edit form
- Translation queue visible at `/admin/content/translations` (editors+)
- Side-by-side translation form: source language fields on left, target language fields on right
- Translation status tracked: `needs_translation`, `in_progress`, `translated`
- SDK features: i18n integration, `tap_form_alter`, `tap_item_view` badge injection, cron processing

### Story 38.4: Italian Content Seed Data

**Status:** Not started

**As a** tutorial reader,
**I want** pre-seeded bilingual content,
**So that** I can see the translation system in action without translating everything manually.

**Acceptance criteria:**

- ~20 Italian tech conferences seeded with Italian names and descriptions
- English translations provided for each seeded Italian conference
- Mix of translation statuses: some "translated," some "needs_translation" to demonstrate the queue
- Language-aware Gather queries filter and display results in the user's preferred language
- `/conferences` shows English content; `/it/conferenze` shows Italian content
- Seeded via SQL or config import, documented for reproducibility

### Story 38.5: REST API Endpoints

**Status:** Not started

**As an** API consumer,
**I want** JSON endpoints for conference data,
**So that** I can build integrations and external tools.

**Acceptance criteria:**

- Read endpoints: `GET /api/v1/conferences`, `/api/v1/conferences/{id}`, `/api/v1/topics`, `/api/v1/topics/{id}/conferences`, `/api/v1/search?q=`, `/api/v1/speakers`, `/api/v1/speakers/{id}`
- Write endpoints: `POST /api/v1/conferences` (editor+), `PATCH /api/v1/conferences/{id}` (editor+)
- Subscription endpoints: `POST /api/v1/conferences/{id}/subscribe` (authenticated), `DELETE /api/v1/conferences/{id}/subscribe`
- Gather engine powers read endpoints -- same queries, filters, and pagination as HTML pages
- Pagination: `?page=`, `?per_page=` parameters; response includes `total`, `page`, `per_page`
- Filtering: `?topic=`, `?country=`, `?online=`, `?lang=` on conference list
- Stage awareness: Live by default; `?stage=curated` available to editors+
- Language awareness: `?lang=it` returns Italian translations when available
- Consistent JSON error format: `{"error": "...", "status": 404}`

### Story 38.6: API Authentication & Rate Limiting

**Status:** Not started

**As a** site administrator,
**I want** API access controlled by tokens and rate limits,
**So that** the API is secure and protected from abuse.

**Acceptance criteria:**

- API key authentication via `Authorization: Bearer {key}` header
- API keys manageable in user profile (generate, revoke, list active keys)
- Each API key tied to a user -- requests inherit the user's role and permissions
- Anonymous API access allowed for read-only endpoints (no Bearer token)
- Rate limiting via Tower middleware: anonymous 60 req/min, authenticated 300 req/min
- Rate limit response: HTTP 429 with `Retry-After` header
- Rate limit state stored in Redis, keyed by API key or IP
- API key validation cached to avoid per-request database lookups

---

## Payoff

A global, API-ready conference platform. The reader understands:

- How Trovato handles multilingual content with JSONB parallel field sets (no separate translation tables)
- How language-prefixed routing and pathauto generate localized URLs with `hreflang` SEO tags
- How the `ritrovo_translate` plugin provides a complete translation editorial workflow
- How the Gather engine's query layer naturally becomes a REST API through JSON serialization
- How API authentication via Bearer tokens reuses the same role/permission model as the web UI
- How rate limiting protects the API from abuse with per-role configurable limits

Five plugins now collaborate across the Ritrovo ecosystem: `ritrovo_importer` (Part 2) feeds data, `ritrovo_access` (Part 5) controls visibility, `ritrovo_cfp` (Part 5) tracks deadlines, `ritrovo_notify` (Part 6) delivers notifications, and `ritrovo_translate` (Part 7) bridges languages. Each is independent, each extends the kernel through taps, and together they deliver a feature set that rivals platforms built by teams of dozens.

---

## What's Deferred

These are explicitly **not** in Part 7 (and the tutorial should say so):

- **Caching & performance** -- Part 8 (tag-based invalidation, L1/L2 cache, Gander profiling)
- **Batch operations** -- Part 8 (bulk publish, bulk re-import)
- **S3 storage** -- Part 8 (production file storage backend)
- **Test suite** -- Part 8 / Epic 15 (comprehensive integration tests)
- **LLM-powered auto-translation** -- Epic 3 (optional, disabled by default; manual workflow is the primary path)
- **More languages** -- Future (the architecture supports N languages; the tutorial demonstrates two)
- **API versioning** -- Future (v1 prefix is forward-looking; versioning strategy deferred)
- **OpenAPI/Swagger specification** -- Future (documentation is manual in this part)
- **GraphQL** -- Future (REST is the primary API; GraphQL is a potential plugin)
- **Personalized search language bias** -- Future (search results in preferred language ranked higher)

---

## Related

- [Ritrovo Overview](overview.md)
- [Documentation Architecture](documentation-architecture.md)
- [Epic 8: Community & Plugin Communication](epic-08.md) -- Part 6 comments, subscriptions, notifications
- [Epic 3: AI as a Building Block](epic-03.md) -- AI-powered translation (optional)
- [Web Layer Design](../design/Design-Web-Layer.md)
- [Plugin SDK Design](../design/Design-Plugin-SDK.md)
- [Infrastructure Design](../design/Design-Infrastructure.md)
