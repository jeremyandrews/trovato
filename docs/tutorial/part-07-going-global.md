# Part 7: Going Global

Part 6 gave Ritrovo a community: threaded comments, subscriptions, and notifications powered by three collaborating plugins. Part 7 breaks the language barrier and opens a programmatic API.

Ritrovo goes **bilingual** (English + Italian), gets a **translation workflow** powered by the fifth and final Ritrovo plugin, and exposes a **JSON REST API** so external tools can consume conference data programmatically.

**Start state:** English-only, existing API at `/api/`, four plugin designs (`ritrovo_importer`, `ritrovo_cfp`, `ritrovo_access`, `ritrovo_notify`).
**End state:** Bilingual site (English + Italian), translation editorial workflow, versioned REST API with authentication and rate limiting, five plugins.

> **Implementation note:** The kernel already has a language model (`models/language.rs`) with BCP 47 validation and a language middleware skeleton (`middleware/language.rs`). API token authentication (`middleware/api_token.rs`) and existing API endpoints at `/api/` are operational. However, content translation storage, the `ritrovo_translate` plugin, Italian seed data, i18n URL routing, and the `/api/v1/` versioned endpoints described here are not yet implemented. This part walks through their design alongside the infrastructure that already exists.

---

## Step 1: i18n Architecture

Trovato supports multilingual content through JSONB parallel field sets. Instead of separate translation tables, translatable fields store values for each language within the same JSONB structure.

### Bilingual Configuration

Configure Trovato for two languages:
- **English** (default) -- no URL prefix
- **Italian** -- `/it/` URL prefix

The language configuration lives in `docs/tutorial/config/language.en.yml` and `docs/tutorial/config/language.it.yml`:

```yaml
# language.en.yml
id: en
label: English
direction: ltr
is_default: true
weight: 0
```

```yaml
# language.it.yml
id: it
label: Italiano
direction: ltr
is_default: false
weight: 1
```

Import them:

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config
```

### Content Translation Model

Translatable fields store parallel values for each language:

```json
{
  "field_description": {
    "en": {
      "value": "<p>A Rust conference in Portland.</p>",
      "format": "filtered_html"
    },
    "it": {
      "value": "<p>Una conferenza su Rust a Portland.</p>",
      "format": "filtered_html"
    }
  }
}
```

Not all fields are translatable. The distinction:

| Translatable | Language-Neutral |
|---|---|
| Title, description, city | Dates, URLs, booleans |
| Bio, body text | Coordinates, prices |
| Country (localized name) | UUIDs, references |

The Item Type definition marks which fields are translatable. Language-neutral fields store a single value regardless of the active language.

### Language Detection

The kernel determines the active language from (in order):

1. **URL prefix** -- `/it/conferenze/rustconf-2026` → Italian
2. **User preference cookie** -- `lang=it` cookie from previous visit
3. **`Accept-Language` header** -- browser preference for first-time visitors
4. **Default** -- English (the `is_default` language)

### Language Switcher

The site header includes a language switcher: "English / Italiano" links. Each link points to the equivalent page in the other language:

```html
<a href="/conferences/rustconf-2026" hreflang="en">English</a>
<a href="/it/conferenze/rustconf-2026" hreflang="it">Italiano</a>
```

### Locale Files

UI strings (button labels, navigation, error messages, form labels) are loaded from locale files. These are JSON or `.po` format files that map English strings to Italian translations:

```json
{
  "Subscribe": "Iscriviti",
  "Search": "Cerca",
  "Upcoming Conferences": "Prossime conferenze",
  "Submit a Conference": "Proponi una conferenza"
}
```

### Verify

```bash
# Languages configured
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id, label, is_default FROM language ORDER BY weight;"
# Expect: en (English, default), it (Italiano)
```

<details>
<summary>Under the Hood: Language Middleware</summary>

The language middleware (`middleware/language.rs`) runs early in the request pipeline. It:

1. Extracts the language prefix from the URL path (`/it/...` → `it`)
2. Falls back to the `lang` cookie, then `Accept-Language` header, then the default language
3. Stores the active language in the request extensions
4. Strips the language prefix from the path before route matching

This means routes do not need language-aware path definitions. `/conferences` and `/it/conferences` both match the same route handler -- the middleware handles the prefix transparently.

The language is available in Tera templates as `{{ active_language }}` and in request state for plugins via the request context.

</details>

---

## Step 2: Translated URL Aliases

Each language gets clean, localized URL aliases. English uses bare paths; Italian uses the `/it/` prefix with translated path segments.

### Pathauto Patterns Per Language

| Language | Pattern | Example |
|---|---|---|
| English | `conferences/[title]` | `/conferences/rustconf-2026` |
| Italian | `it/conferenze/[title]` | `/it/conferenze/rustconf-2026` |

The `[title]` token uses the title in the respective language. If the Italian title is "RustConf 2026" (unchanged), the slug is the same; if it is "Conferenza Rust", the slug reflects that.

### `hreflang` Tags

For SEO, each page includes `<link rel="alternate">` tags pointing to equivalent pages in other languages:

```html
<head>
  <link rel="alternate" hreflang="en" href="/conferences/rustconf-2026">
  <link rel="alternate" hreflang="it" href="/it/conferenze/rustconf-2026">
  <link rel="alternate" hreflang="x-default" href="/conferences/rustconf-2026">
</head>
```

The `x-default` tag tells search engines which version to show when no language match exists.

### URL Resolution with Language Prefix

The URL alias middleware handles language prefixes:

1. Request for `/it/conferenze/rustconf-2026`
2. Language middleware extracts `it`, strips prefix → `/conferenze/rustconf-2026`
3. URL alias middleware resolves the alias to the item UUID
4. Item handler loads the item and renders it in Italian

The alias resolution system stores language-specific aliases with a `language` column (foreign key to the `language` table), so the same item can have different aliases in different languages.

### Verify

```bash
# Check for language-specific URL aliases
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT alias, language FROM url_alias WHERE language != 'en' LIMIT 10;"
```

---

## Step 3: The `ritrovo_translate` Plugin

The `ritrovo_translate` plugin is the fifth and final Ritrovo plugin. It provides a translation workflow: language detection, translation flagging, and an editorial queue for translators.

> **Not yet implemented.** The `ritrovo_translate` plugin source does not exist yet. This step describes its design. When written, it will live at `plugins/ritrovo_translate/`.

### What It Does

| Tap | Behavior |
|---|---|
| `tap_item_insert` | Detects language from text fields; sets `translation_status` metadata |
| `tap_item_view` | Adds language indicator badge; shows "View in English / Vedi in italiano" switcher |
| `tap_cron` | Processes translation queue; checks for items flagged as needing translation |
| `tap_form_alter` | Adds language selector dropdown to conference edit form |

### Translation Status

Each item tracks its translation state:

| Status | Meaning |
|---|---|
| `needs_translation` | Item exists in one language but not the other |
| `in_progress` | A translator is working on it |
| `translated` | Both language versions are complete |

The status is stored in the item's metadata (JSONB `data` field on the item).

### Translation Queue

The translation queue at `/admin/content/translations` shows items needing translation:

| Column | Content |
|---|---|
| Title | Item title in source language |
| Type | Content type |
| Source Language | Detected language of the original |
| Status | `needs_translation`, `in_progress`, `translated` |
| Actions | Translate, Mark Complete |

### Side-by-Side Translation Form

Clicking "Translate" opens a side-by-side form:
- Left column: source language fields (read-only)
- Right column: target language fields (editable)
- Save stores the translation in the JSONB parallel field structure

### Language Detection

On `tap_item_insert`, the plugin examines text fields (title, description) to detect the primary language. For the tutorial, detection uses simple heuristics (common Italian words, character patterns). A production system might use a language detection library or the AI capabilities from Epic 3.

### Building the Plugin

```bash
cd plugins/ritrovo_translate
cargo build --target wasm32-wasip1 --release
cp target/wasm32-wasip1/release/ritrovo_translate.wasm ../../plugin-dist/
cargo run --release --bin trovato -- plugin install plugin-dist/ritrovo_translate.wasm
```

### SDK Features Used

- `tap_item_insert` -- Language detection and status flagging
- `tap_item_view` -- Language badge and switcher injection
- `tap_cron` -- Translation queue processing
- `tap_form_alter` -- Language selector injection on edit forms
- i18n integration -- Reading the active language from request context

### Verify

```bash
# Plugin installed
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT name, status FROM plugins WHERE name = 'ritrovo_translate';"
# Expect: ritrovo_translate, 1
```

---

## Step 4: Seeding Italian Content

To demonstrate the bilingual system, seed approximately 20 Italian tech conferences with hand-written English translations.

### Seed Data

The seed data includes Italian conferences with:
- Italian names and descriptions (e.g., "Conferenza Italiana su Rust", "Incontro DevOps Milano")
- English translations for each
- A mix of translation statuses to populate the translation queue

Import the seed data (the seed directory will be created as part of this tutorial step):

```bash
# Create seed-italian/ directory with conference YAML files first, then import:
cargo run --release --bin trovato -- config import docs/tutorial/config/seed-italian
```

> **Not yet created.** The `docs/tutorial/config/seed-italian/` directory and its seed data files do not exist yet. They will be created when this tutorial step is implemented.

### Translation Status Mix

| Count | Status | Purpose |
|---|---|---|
| ~12 | `translated` | Both languages complete -- appear in both `/conferences` and `/it/conferenze` |
| ~5 | `needs_translation` | Italian only -- appear in the translation queue |
| ~3 | `in_progress` | Partially translated -- demonstrate the workflow |

### Language-Aware Gathers

The existing Gather queries become language-aware:
- `/conferences` shows conference titles and descriptions in English
- `/it/conferenze` shows the same conferences with Italian titles and descriptions
- If a translation is missing, the default language (English) content is displayed as fallback

### Verify

```bash
# Italian conferences seeded
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT COUNT(*) FROM item WHERE type = 'conference' AND data->>'primary_language' = 'it';"
# Expect: ~20

# English content accessible
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/conferences
# 200

# Italian content accessible
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/it/conferenze
# 200
```

---

## Step 5: REST API

The same Gather engine that powers HTML pages also powers a JSON API. The API is a thin serialization layer on top of existing queries.

### API Endpoints

| Method | Path | Description | Auth |
|---|---|---|---|
| `GET` | `/api/v1/conferences` | List upcoming conferences | Public |
| `GET` | `/api/v1/conferences/{id}` | Single conference with all fields | Public |
| `GET` | `/api/v1/topics` | Topic category hierarchy | Public |
| `GET` | `/api/v1/topics/{id}/conferences` | Conferences for a topic | Public |
| `GET` | `/api/v1/search?q=` | Full-text search | Public |
| `GET` | `/api/v1/speakers` | List speakers | Public |
| `GET` | `/api/v1/speakers/{id}` | Speaker with linked conferences | Public |
| `POST` | `/api/v1/conferences` | Create conference | Editor+ |
| `PATCH` | `/api/v1/conferences/{id}` | Update conference | Editor+ |
| `POST` | `/api/v1/conferences/{id}/subscribe` | Subscribe | Authenticated |
| `DELETE` | `/api/v1/conferences/{id}/subscribe` | Unsubscribe | Authenticated |

### Query Parameters

Read endpoints support:

| Parameter | Example | Effect |
|---|---|---|
| `?topic=rust` | Filter by topic slug | Topic + descendants |
| `?country=US` | Filter by country | Exact match |
| `?online=true` | Online events only | Boolean filter |
| `?lang=it` | Italian content | Returns Italian translations |
| `?stage=curated` | Stage filter | Editor+ only |
| `?page=2&per_page=20` | Pagination | Offset-based |
| `?q=rust` | Search query | Full-text search |

### Authentication

API authentication uses Bearer tokens:

```
Authorization: Bearer trovato_api_sk_abc123def456
```

API keys are managed in the user profile. Each key is tied to a user -- requests inherit the user's roles and permissions.

Anonymous API access is allowed for read-only endpoints (no `Authorization` header required).

### Rate Limiting

| Tier | Limit | Scope |
|---|---|---|
| Anonymous | 60 requests/minute | Per IP |
| Authenticated | 300 requests/minute | Per API key |

Rate limit exceeded returns HTTP 429 with a `Retry-After` header. Rate limit state is stored in Redis, keyed by API key or IP address.

### Response Format

Successful responses:

```json
{
  "data": [...],
  "total": 142,
  "page": 1,
  "per_page": 20
}
```

Error responses:

```json
{
  "error": "Conference not found",
  "status": 404
}
```

### Stage Awareness

The API respects stage visibility:
- Anonymous requests see only Live content
- Authenticated requests with editor+ permissions can use `?stage=curated` to see internal content
- The same `stage_aware` Gather logic from Part 4 applies

### Language Awareness

The `?lang=it` parameter returns Italian translations when available. If a translation is missing, the response falls back to the default language content. The `Content-Language` response header indicates which language was used.

### Verify

```bash
# List conferences
curl -s http://localhost:3000/api/v1/conferences | jq '.total'
# Returns count of Live conferences

# Single conference
ID=$($(brew --prefix libpq)/bin/psql -tA postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id FROM item WHERE type = 'conference' AND status = 1 LIMIT 1;")
curl -s http://localhost:3000/api/v1/conferences/$ID | jq '.data.title'

# Search
curl -s 'http://localhost:3000/api/v1/search?q=rust' | jq '.total'

# Topics
curl -s http://localhost:3000/api/v1/topics | jq '.data | length'

# Italian content
curl -s 'http://localhost:3000/api/v1/conferences?lang=it' | jq '.data[0].title'
```

<details>
<summary>Under the Hood: API Token Architecture</summary>

API tokens are stored in the `api_tokens` table:

```sql
CREATE TABLE api_tokens (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    token_hash VARCHAR(64) NOT NULL UNIQUE,
    created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_used TIMESTAMPTZ,
    expires_at TIMESTAMPTZ
);
```

Tokens are hashed with SHA-256 before storage -- the raw token is shown only once at creation. The middleware (`middleware/api_token.rs`) extracts the Bearer token, hashes it, looks up the user, and injects the user context into the request.

Token validation is cached to avoid per-request database lookups. The cache entry is keyed by the token hash and has a short TTL (60 seconds by default).

Token CRUD is available at `/api/tokens` (authenticated via session) and as a self-service page in the user profile.

</details>

---

## Step 6: API Documentation & Testing

Verify all endpoints work correctly with curl.

### Endpoint Testing

```bash
# List conferences (paginated)
curl -s 'http://localhost:3000/api/v1/conferences?per_page=5' | jq '.data | length'
# Expect: <= 5

# Filter by topic
curl -s 'http://localhost:3000/api/v1/conferences?topic=rust' | jq '.total'

# Single conference with all fields
curl -s http://localhost:3000/api/v1/conferences/$ID | jq '.data | keys'
# Expect: includes title, type, fields, created, etc.

# Speakers
curl -s http://localhost:3000/api/v1/speakers | jq '.total'

# Search
curl -s 'http://localhost:3000/api/v1/search?q=conference' | jq '.total'

# Topics
curl -s http://localhost:3000/api/v1/topics | jq '.data[0]'
```

### Error Cases

```bash
# 404 for missing item
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/api/v1/conferences/00000000-0000-0000-0000-000000000000
# 404

# 403 for unauthorized stage access
curl -s -o /dev/null -w "%{http_code}" 'http://localhost:3000/api/v1/conferences?stage=curated'
# 403 (anonymous cannot access curated)
```

### Authenticated API Access

```bash
# Create an API token (via user profile or directly)
# Then use it:
curl -s -H "Authorization: Bearer trovato_api_sk_..." \
  'http://localhost:3000/api/v1/conferences?stage=curated' | jq '.total'
# Returns curated conferences (if user has editor+ permissions)
```

### Rate Limiting

```bash
# Rapid requests to trigger rate limit
for i in $(seq 1 65); do
  curl -s -o /dev/null -w "%{http_code}\n" http://localhost:3000/api/v1/conferences
done
# After 60 requests: 429
```

### Verify

```bash
# API health check
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/api/v1/conferences
# 200

# API token table exists
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "\d api_tokens"
# Expect: id, user_id, token_hash, name, created, last_used, expires_at
```

---

## What You've Built

By the end of Part 7, you have:

- **Bilingual site** (English + Italian) with JSONB parallel field sets, language-prefixed URLs, a language switcher, and `hreflang` SEO tags.
- **`ritrovo_translate` plugin** with language detection, translation status tracking, side-by-side translation form, and `tap_form_alter` for language selector injection.
- **Italian seed data** -- ~20 Italian conferences with English translations, demonstrating the translation queue with mixed statuses.
- **REST API** -- 11 endpoints (7 read, 2 write, 2 subscription) powered by the same Gather engine and access control as the HTML pages.
- **API authentication** via Bearer tokens with per-user permissions and Redis-backed rate limiting.
- **Language-aware API** with `?lang=it` parameter and `Content-Language` response headers.

You also now understand:

- How Trovato handles multilingual content with JSONB parallel field sets (no separate translation tables).
- How language-prefixed routing and pathauto generate localized URLs with `hreflang` SEO tags.
- How the `ritrovo_translate` plugin provides a translation editorial workflow.
- How the Gather engine's query layer naturally becomes a REST API through JSON serialization.
- How API authentication via Bearer tokens reuses the same role/permission model as the web UI.
- How rate limiting protects the API from abuse with per-tier configurable limits.

Five plugins now collaborate across the Ritrovo ecosystem: `ritrovo_importer` (Part 2) feeds data, `ritrovo_cfp` (Part 5) tracks deadlines, `ritrovo_access` (Part 5) controls visibility, `ritrovo_notify` (Part 6) delivers notifications, and `ritrovo_translate` (Part 7) bridges languages. Each is independent, each extends the kernel through taps, and together they deliver a feature set that rivals platforms built by teams of dozens.

---

## What's Deferred

| Feature | Deferred To | Reason |
|---|---|---|
| Caching & performance | Part 8 | Tag-based invalidation, L1/L2 cache, Gander profiling |
| Batch operations | Part 8 | Bulk publish, bulk re-import |
| S3 storage | Part 8 | Production file storage backend |
| Test suite | Part 8 | Comprehensive integration tests |
| LLM-powered auto-translation | Future | Optional, disabled by default; manual workflow is the primary path |
| More languages | Future | Architecture supports N languages; tutorial demonstrates two |
| API versioning | Future | v1 prefix is forward-looking; versioning strategy deferred |
| OpenAPI/Swagger spec | Future | Documentation is manual in this part |
| GraphQL | Future | REST is the primary API; GraphQL is a potential plugin |
| Personalized search language bias | Future | Search results in preferred language ranked higher |

---

## Related

- [Part 6: Community & Plugin Communication](part-06-community.md)
- [Plugin SDK Design](../design/Design-Plugin-SDK.md)
- [Web Layer Design](../design/Design-Web-Layer.md)
- [Infrastructure Design](../design/Design-Infrastructure.md)
- [Epic 9: Going Global](../ritrovo/epic-09.md)
