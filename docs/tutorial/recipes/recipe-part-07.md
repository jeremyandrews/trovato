# Recipe: Part 7 — Going Global

> **Synced with:** `docs/tutorial/part-07-going-global.md`
> **Sync hash:** 3c577b34
> **Last verified:** 2026-03-15
>
> Run `docs/tutorial/recipes/sync-check.sh` before starting to verify this recipe matches the current tutorial.

---

## Prerequisites

- Parts 1–6 must be completed (comments, subscriptions, ritrovo_notify plugin, three-plugin collaboration).
- Check `TOOLS.md` for server start commands, database connection, admin credentials, plugin build commands.
- Database backup recommended:

```bash
$(brew --prefix libpq)/bin/pg_dump \
  postgres://trovato:trovato@localhost:5432/trovato \
  -Fc -f backups/pre-part-07-$(date +%Y%m%d).dump
```

---

## Step 1: i18n Architecture

### 1.1 Create Language Config Files

`[CLI]` Create the language configuration files if they do not already exist:

```bash
cat docs/tutorial/config/language.en.yml
cat docs/tutorial/config/language.it.yml
```

**Verify:** Both files exist with `id`, `label`, `direction`, `is_default`, `weight` fields.

### 1.2 Import Language Configuration

`[CLI]`

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config
```

**Verify:** Config imported successfully.

### 1.3 Verify Languages Configured

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id, label, is_default FROM language ORDER BY weight;"
```

**Verify:** Two rows: en (English, default=true), it (Italiano, default=false).

### 1.4 Understand Translation Model

`[REFERENCE]` Key concepts:
- Translatable fields: JSONB parallel field sets — `{ "en": { "value": "..." }, "it": { "value": "..." } }`
- Language-neutral fields: dates, URLs, booleans — single value regardless of language
- Language detection order: URL prefix → cookie → Accept-Language → default
- Language switcher: "English / Italiano" links in site header
- Locale files: JSON or .po format for UI string translations

### 1.5 Test Language Switcher

`[CLI]`

```bash
# English content (no prefix)
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/conferences
# Expect: 200

# Italian content (with /it/ prefix)
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/it/conferenze
# Expect: 200 (after Italian aliases are created)
```

Record language configuration in `TOOLS.md -> i18n`.

---

## Step 2: Translated URL Aliases

### 2.1 Understand URL Alias Translation

`[REFERENCE]` Key concepts:
- Pathauto patterns per language: `conferences/[title]` (EN), `it/conferenze/[title]` (IT)
- URL alias table has `language` column for language-specific aliases
- `hreflang` tags generated in page `<head>` for SEO
- Language middleware strips `/it/` prefix before route matching
- Same item can have different aliases in different languages

### 2.2 Verify Language-Specific Aliases

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT alias, language FROM url_alias WHERE language IS NOT NULL ORDER BY alias LIMIT 10;"
```

**Verify:** Aliases exist with language codes after Italian content is seeded (Step 4).

### 2.3 Verify hreflang Tags

`[CLI]` After Italian content exists:

```bash
curl -s http://localhost:3000/conferences | grep -c 'hreflang'
# Expect: >= 1 (alternate language tags present)
```

---

## Step 3: The `ritrovo_translate` Plugin

### 3.1 Review Plugin Design

`[REFERENCE]` Key taps:
- `tap_item_insert`: language detection, sets `translation_status` metadata
- `tap_item_view`: language badge, "View in English / Vedi in italiano" switcher
- `tap_cron`: translation queue processing
- `tap_form_alter`: language selector dropdown on edit forms
- Translation statuses: `needs_translation`, `in_progress`, `translated`
- Translation queue at `/admin/content/translations`
- Side-by-side translation form: source (read-only) | target (editable)

### 3.2 Build the Plugin

> **Not yet implemented.** The `ritrovo_translate` plugin source does not exist yet. Skip steps 3.2–3.5 until it is written.

`[CLI]`

```bash
cd plugins/ritrovo_translate
cargo build --target wasm32-wasip1 --release
cp target/wasm32-wasip1/release/ritrovo_translate.wasm ../../plugin-dist/
```

**Verify:** `plugin-dist/ritrovo_translate.wasm` exists.

### 3.3 Install the Plugin

`[CLI]`

```bash
cargo run --release --bin trovato -- plugin install plugin-dist/ritrovo_translate.wasm
```

### 3.4 Verify Plugin Installation

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT name, status FROM plugins WHERE name = 'ritrovo_translate';"
```

**Verify:** ritrovo_translate, status 1 (enabled).

### 3.5 Test Translation Queue

`[UI-ONLY]` Navigate to `/admin/content/translations`:
1. Verify items with `needs_translation` status appear
2. Click "Translate" on one
3. Verify side-by-side form shows source and target fields

Record plugin commands in `TOOLS.md -> Plugins`.

---

## Step 4: Seeding Italian Content

### 4.1 Import Italian Seed Data

> **Not yet created.** The `docs/tutorial/config/seed-italian/` directory does not exist yet. Skip steps 4.1–4.4 until the seed data is created.

`[CLI]`

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config/seed-italian
```

**Verify:** Import completes without errors.

### 4.2 Verify Italian Conferences Seeded

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT COUNT(*) FROM item WHERE type = 'conference' AND data->>'primary_language' = 'it';"
# Expect: ~20
```

### 4.3 Verify Translation Status Mix

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT data->>'translation_status' AS status, COUNT(*) FROM item
      WHERE data->>'primary_language' = 'it'
      GROUP BY data->>'translation_status';"
```

**Verify:** Mix of `translated` (~12), `needs_translation` (~5), `in_progress` (~3).

### 4.4 Test Language-Aware Rendering

`[CLI]`

```bash
# English conferences page
curl -s http://localhost:3000/conferences | grep -c 'conference'
# Returns English content

# Italian conferences page
curl -s http://localhost:3000/it/conferenze | grep -c 'conferenza'
# Returns Italian content (after aliases exist)
```

---

## Step 5: REST API

> **Note:** The `/api/v1/` versioned endpoints below are not yet implemented. Existing API endpoints are at `/api/` (without version prefix). These steps describe the planned versioned API design. To test existing endpoints now, use `/api/items`, `/api/item/{id}`, `/api/search`, `/api/categories`, etc.

### 5.1 Test Read Endpoints

`[CLI]`

```bash
# List conferences
curl -s http://localhost:3000/api/v1/conferences | jq '.total'

# Single conference
ID=$($(brew --prefix libpq)/bin/psql -tA postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id FROM item WHERE type = 'conference' AND status = 1 LIMIT 1;")
curl -s http://localhost:3000/api/v1/conferences/$ID | jq '.data.title'

# Search
curl -s 'http://localhost:3000/api/v1/search?q=rust' | jq '.total'

# Topics
curl -s http://localhost:3000/api/v1/topics | jq '.data | length'

# Speakers
curl -s http://localhost:3000/api/v1/speakers | jq '.total'
```

### 5.2 Test Pagination

`[CLI]`

```bash
curl -s 'http://localhost:3000/api/v1/conferences?per_page=5&page=2' | jq '{total: .total, page: .page, per_page: .per_page, count: (.data | length)}'
```

**Verify:** Response includes `total`, `page`, `per_page` metadata with <= 5 items in `data`.

### 5.3 Test Filtering

`[CLI]`

```bash
# By topic
curl -s 'http://localhost:3000/api/v1/conferences?topic=rust' | jq '.total'

# By country
curl -s 'http://localhost:3000/api/v1/conferences?country=US' | jq '.total'

# Online only
curl -s 'http://localhost:3000/api/v1/conferences?online=true' | jq '.total'
```

### 5.4 Test Language Parameter

`[CLI]`

```bash
# Italian content via API
curl -s 'http://localhost:3000/api/v1/conferences?lang=it' | jq '.data[0].title'
```

### 5.5 Test Error Cases

`[CLI]`

```bash
# 404 for missing item
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/api/v1/conferences/00000000-0000-0000-0000-000000000000
# 404

# 403 for unauthorized stage access
curl -s -o /dev/null -w "%{http_code}" 'http://localhost:3000/api/v1/conferences?stage=curated'
# 403
```

### 5.6 Test API Token Table

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "\d api_tokens"
```

**Verify:** Table exists with id, user_id, token_hash, name, created, last_used, expires_at columns.

Record API endpoints and testing commands in `TOOLS.md -> REST API`.

---

## Step 6: API Documentation & Testing

### 6.1 Test Authenticated API Access

`[CLI]` Create an API token and test authenticated endpoints:

```bash
# Create a token via the user profile or API
# Then test with Bearer auth:
# curl -s -H "Authorization: Bearer trovato_api_sk_..." \
#   'http://localhost:3000/api/v1/conferences?stage=curated' | jq '.total'
```

### 6.2 Test Rate Limiting

`[CLI]`

```bash
# Rapid requests (anonymous limit: 60/min)
for i in $(seq 1 65); do
  STATUS=$(curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/api/v1/conferences)
  if [ "$STATUS" = "429" ]; then
    echo "Rate limited at request $i"
    break
  fi
done
```

**Verify:** 429 returned after ~60 requests.

### 6.3 Verify Content Negotiation

`[CLI]`

```bash
curl -s -H "Accept: application/json" http://localhost:3000/api/v1/conferences | jq '.total'
# Returns JSON
```

---

## Completion Checklist

```bash
echo "=== Part 7 Completion Checklist ==="
echo -n "1. Languages configured: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COUNT(*) FROM language;" | tr -d ' '
echo -n "2. Italian conferences: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COUNT(*) FROM item WHERE type = 'conference' AND data->>'primary_language' = 'it';" | tr -d ' '
echo -n "3. ritrovo_translate: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COALESCE((SELECT status::text FROM plugins WHERE name = 'ritrovo_translate'), 'not installed');" | tr -d ' '
echo -n "4. All 5 plugins: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COUNT(*) FROM plugins WHERE name IN ('ritrovo_importer', 'ritrovo_cfp', 'ritrovo_access', 'ritrovo_notify', 'ritrovo_translate') AND status = 1;" | tr -d ' '
echo -n "5. API conferences: "; curl -s http://localhost:3000/api/items?type=conference | jq -r '.total // "error"'
echo -n "6. API tokens table: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT CASE WHEN EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'api_tokens') THEN 'yes' ELSE 'no' END;" | tr -d ' '
echo ""
```

Expected output:
```
1. Languages configured: 2
2. Italian conferences: ~20 (or 0 if seed data not yet created)
3. ritrovo_translate: 1 (or "not installed" if plugin not yet written)
4. All 5 plugins: 5 (or fewer if plugins not yet written)
5. API conferences: > 0 (uses existing /api/ endpoint)
6. API tokens table: yes
```

Create a database backup:
```bash
$(brew --prefix libpq)/bin/pg_dump \
  postgres://trovato:trovato@localhost:5432/trovato \
  -Fc -f backups/tutorial-part-07-$(date +%Y%m%d).dump
```

Record backup in `TOOLS.md -> Backups`.

All discoveries should be recorded in `TOOLS.md`.
