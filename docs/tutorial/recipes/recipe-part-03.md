# Recipe: Part 3 — Look & Feel

> **Synced with:** `docs/tutorial/part-03-look-and-feel.md`
> **Sync hash:** cb61177f
> **Last verified:** 2026-03-12
>
> Run `docs/tutorial/recipes/sync-check.sh` before starting to verify this recipe matches the current tutorial.

---

## Prerequisites

- Parts 1 and 2 must be completed (conference type, importer, taxonomy, gathers all working).
- Check `TOOLS.md` for server start commands, database connection string, admin URL, config import commands, and plugin build commands.
- Database backup recommended before starting:

```bash
$(brew --prefix libpq)/bin/pg_dump \
  postgres://trovato:trovato@localhost:5432/trovato \
  -Fc -f backups/pre-part-03-$(date +%Y%m%d).dump
```

---

## Step 1: The Render Tree & Tera Templates

### 1.1 Understand the Render Tree

`[REFERENCE]` No action needed. Key concepts:
- Four phases: Build → Alter → Sanitize → Render
- Plugins never produce raw HTML — they produce structured data
- Template resolution chain: `item--{type}--{id}` → `item--{type}` → `item`
- `safe_urls` pattern prevents `javascript:` URI injection in `href` attributes

### 1.2 Verify Templates Ship with the Project

`[CLI]` The conference and speaker templates already exist in the repo:

```bash
ls templates/elements/item--conference.html templates/elements/item--speaker.html templates/gather/query--ritrovo.open_cfps.html
```

**Verify:** All three files exist.

### 1.3 Inspect Conference Detail Template

`[CLI]` Confirm the conference template uses safe_urls for external links:

```bash
grep -c 'safe_urls' templates/elements/item--conference.html
```

**Verify:** Returns > 0 (safe_urls used for field_url, field_cfp_url).

### 1.4 Verify Conference Detail Rendering

`[CLI]` Pick a conference and check the detail page renders with the new template:

```bash
ID=$(curl -s http://localhost:3000/api/query/ritrovo.upcoming_conferences/execute | jq -r '.items[0].id')
curl -s http://localhost:3000/item/$ID | grep -o 'class="conf-detail[^"]*"' | head -5
```

**Verify:** CSS classes like `conf-detail__header`, `conf-detail__meta`, `conf-detail__desc` present.

### 1.5 Verify Open CFPs Template

`[CLI]`

```bash
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/cfps
# Expect: 200

curl -s http://localhost:3000/cfps | grep -c 'class="cfp-card'
# Expect: > 0 (if open CFPs exist)
```

### 1.6 Inspect Page Layout

`[CLI]` Verify the page template has slot regions:

```bash
grep -c 'page-sidebar\|page-footer\|site-header\|site-nav' templates/page.html
```

**Verify:** Returns > 0. Record template paths in `TOOLS.md -> Templates`.

---

## Step 2: File Uploads & Media

### 2.1 Verify File Fields in Conference Config

`[CLI]` The conference YAML config should include file fields:

```bash
grep 'field_logo\|field_venue_photo' docs/tutorial/config/item_type.conference.yml
```

**Verify:** Both fields listed.

### 2.2 Import Updated Config

`[CLI]`

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config
```

**Verify:** Output shows config entities imported (count may vary based on what's new vs already imported).

Wait for cache TTL:

```bash
sleep 5
```

### 2.3 Verify File Upload Endpoint

`[CLI]` The file upload endpoint exists (POST-only, multipart):

```bash
curl -s -o /dev/null -w "%{http_code}" -X POST http://localhost:3000/file/upload -F "dummy=test"
# Expect: 401 (unauthorized — requires auth, needs multipart encoding)

curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/file/upload
# Expect: 405 (GET not allowed — POST only)
```

### 2.4 Upload a File via Admin

`[UI-ONLY]` Navigate to `/admin/content`, edit an existing conference, and upload an image to the Logo field. Save the form.

Alternatively, `[CLI]` upload via curl:

```bash
# Login first (see TOOLS.md -> Admin UI for login flow)
CSRF=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  http://localhost:3000/admin | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')

# Upload a test image (create a small PNG)
printf '\x89PNG\r\n\x1a\n' > /tmp/test-logo.png
curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/file/upload \
  -H "X-CSRF-Token: $CSRF" \
  -F "file=@/tmp/test-logo.png" | jq '.success'
# Expect: true
```

### 2.5 Verify File Record

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id, filename, filemime, status FROM file_managed ORDER BY created DESC LIMIT 3;"
```

**Verify:** File records exist. Files attached to saved items have `status = 1` (permanent).

### 2.6 Verify File Serving Security

`[CLI]` Directory traversal is blocked:

```bash
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/files/../etc/passwd
# Expect: 404

curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/files/
# Expect: 308 → /files → 404 (trailing-slash redirect then not found)
```

Record file upload endpoints and allowed MIME types in `TOOLS.md -> Files/Media`.

---

## Step 3: Speaker Content Type

### 3.1 Verify Speaker Config Exists

`[CLI]`

```bash
cat docs/tutorial/config/item_type.speaker.yml | head -5
```

**Verify:** Shows `type: speaker` with field definitions.

### 3.2 Import Config

`[CLI]`

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config
sleep 5

curl -s http://localhost:3000/api/content-types | jq '.[] | select(. == "speaker")'
# Expect: "speaker"
```

### 3.3 Create Speakers

`[UI-ONLY]` Navigate to `/admin/content/add/speaker`. Create 2-3 speakers with:
- A name (title)
- A bio (field_bio)
- A company (field_company)
- Conference references — paste UUIDs from:

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id, title FROM item WHERE type = 'conference' ORDER BY title LIMIT 10;"
```

### 3.4 Verify Speaker Template

`[CLI]` After creating a speaker:

```bash
$(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id FROM item WHERE type = 'speaker' LIMIT 1;" | tr -d ' '
```

Use the returned ID:

```bash
curl -s http://localhost:3000/item/SPEAKER-ID | grep -o 'class="speaker-detail[^"]*"' | head -3
```

**Verify:** CSS classes like `speaker-detail__company`, `speaker-detail__bio` present.

### 3.5 Verify Pathauto for Speakers

`[CLI]` The pathauto pattern for speakers should already be imported:

```bash
$(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT value FROM site_config WHERE key = 'pathauto_patterns';" | jq '.speaker'
# Expect: "speakers/[title]"
```

Regenerate aliases:

```bash
CSRF=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  http://localhost:3000/admin | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')

curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/admin/config/pathauto/regenerate \
  --data-urlencode "_token=$CSRF" \
  --data-urlencode "item_type=speaker" \
  -o /dev/null -w "%{http_code}"
# Expect: 303
```

### 3.6 Verify Reverse References

`[CLI]` On a conference that has speakers linked to it, the conference detail page should show a speakers section:

```bash
# Find a conference referenced by a speaker
$(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT DISTINCT fields->>'field_conferences' FROM item WHERE type = 'speaker' AND fields->>'field_conferences' IS NOT NULL LIMIT 1;" | tr -d ' '
```

Visit that conference's detail page and look for the reverse reference section:

```bash
curl -s http://localhost:3000/item/CONFERENCE-ID | grep -o 'class="conf-detail__speakers[^"]*"'
```

**Verify:** Speakers section present on conference detail page.

---

## Step 4: Page Layout — Slots, Tiles & Navigation

### 4.1 Understand Slot Architecture

`[REFERENCE]` No action needed. Key concepts:
- Five regions: Header, Navigation, Content, Sidebar, Footer
- `inject_site_context()` builds the page context with menus, tiles, auth state
- Tiles have machine_name, region, tile_type, config, visibility, weight
- Menus loaded from `menu_link` table, sorted by weight

### 4.2 Verify Page Template Has Slot Regions

`[CLI]`

```bash
grep -c 'header_tiles\|navigation_tiles\|sidebar_tiles\|footer_tiles' templates/page.html
```

**Verify:** Returns > 0 (tile regions referenced in template).

### 4.3 Verify Menu Rendering

`[CLI]`

```bash
curl -s http://localhost:3000/conferences | grep -o 'class="site-nav[^"]*"' | head -3
```

**Verify:** Navigation classes present (e.g., `site-nav`, `site-nav__link`). If no database menu links exist yet, the template falls back to plugin-registered menus.

### 4.4 Create Menu Links

`[CLI]` Menu links are not yet config-importable. Insert directly via SQL:

```bash
NOW=$(date +%s)
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato <<SQL
INSERT INTO menu_link (menu_name, path, title, weight, created, changed) VALUES
  ('main', '/conferences', 'Conferences', 0, $NOW, $NOW),
  ('main', '/speakers', 'Speakers', 5, $NOW, $NOW),
  ('main', '/cfps', 'Open CFPs', 10, $NOW, $NOW),
  ('main', '/topics', 'Topics', 15, $NOW, $NOW),
  ('footer', '/about', 'About', 0, $NOW, $NOW),
  ('footer', '/contact', 'Contact', 5, $NOW, $NOW);
SQL
```

**Verify:** `INSERT 0 6`. Wait 5s for cache, then confirm navigation appears (see Step 4.6).

### 4.5 Verify Breadcrumbs

`[CLI]`

```bash
ID=$(curl -s http://localhost:3000/api/query/ritrovo.upcoming_conferences/execute | jq -r '.items[0].id')
curl -s http://localhost:3000/item/$ID | grep -o 'class="breadcrumb[^"]*"' | head -1
```

**Verify:** Breadcrumb classes present.

### 4.6 Verify Active Menu Highlighting

`[CLI]`

```bash
curl -s http://localhost:3000/conferences | grep -o 'site-nav__link--active'
```

**Verify:** Active class present on the Conferences menu item when viewing `/conferences`.

### 4.7 Verify Sidebar

`[CLI]` The sidebar region only renders when tiles are assigned to it:

```bash
curl -s http://localhost:3000/conferences | grep -o 'class="page-layout__sidebar[^"]*"' | head -1
```

**Verify:** If tiles exist in the sidebar slot, the region appears. With no tiles configured yet, the sidebar is intentionally empty — that's expected. Tiles are not config-importable; they must be created via admin UI or SQL.

Record tile and menu configuration in `TOOLS.md -> Layout`.

---

## Step 5: Full-Text Search

### 5.1 Verify Search Field Configs Exist

`[CLI]`

```bash
ls docs/tutorial/config/search_field_config.*.yml
```

**Verify:** Six files with UUID-based names (conference title/description/city/country + speaker title/bio).

### 5.2 Import Search Config

`[CLI]`

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config
```

**Verify:** Search field configs imported.

### 5.3 Rebuild Search Index

`[CLI]` There is no CLI subcommand for reindexing. Use the admin endpoint for each content type:

```bash
# Reindex conferences
CSRF=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  http://localhost:3000/admin | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/admin/structure/types/conference/search/reindex \
  -d "_token=$CSRF" -o /dev/null -w "%{http_code}"
# Expect: 303

# Reindex speakers
CSRF=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  http://localhost:3000/admin | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/admin/structure/types/speaker/search/reindex \
  -d "_token=$CSRF" -o /dev/null -w "%{http_code}"
# Expect: 303
```

**Verify:** Both return 303. The reindex touches item timestamps to re-fire the DB trigger that populates `search_vector`.

### 5.4 Verify Search via API

`[CLI]`

```bash
curl -s 'http://localhost:3000/api/search?q=rust' | jq '{total: .total, first: .results[0].title}'
```

**Verify:** Returns results with conference titles matching "rust".

### 5.5 Verify Search Weighting

`[CLI]`

```bash
# Title match (weight A) should rank high
curl -s 'http://localhost:3000/api/search?q=rust' | jq '.results[0].title'
# Expect: Something with "Rust" in the title (e.g., "Rust Belt Rust")

# City match (weight C)
curl -s 'http://localhost:3000/api/search?q=berlin' | jq '.results | length'
# Expect: > 0
```

### 5.6 Verify Search Results Page

`[CLI]`

```bash
curl -s -o /dev/null -w "%{http_code}" 'http://localhost:3000/search?q=rust'
# Expect: 200
```

`[UI-ONLY]` Visit `http://localhost:3000/search?q=rust` in a browser. Confirm results show titles, type badges, and pagination.

Record search commands in `TOOLS.md -> Search`.

---

## Completion Checklist

```bash
echo "=== Part 3 Completion Checklist ==="
echo -n "1. Conference template: "; curl -s http://localhost:3000/item/$(curl -s http://localhost:3000/api/query/ritrovo.upcoming_conferences/execute | jq -r '.items[0].id') | grep -c 'conf-detail'
echo -n "2. CFP template: "; curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/cfps
echo -n "3. Speaker type: "; curl -s http://localhost:3000/api/content-types | jq -r '.[] | select(. == "speaker")'
echo -n "4. File endpoint: "; curl -s -o /dev/null -w "%{http_code}" -X POST http://localhost:3000/file/upload -F "dummy=test"
echo -n "5. Page layout: "; curl -s http://localhost:3000/ | grep -c 'site-header\|page-sidebar\|page-footer'
echo -n "6. Breadcrumbs: "; curl -s http://localhost:3000/item/$(curl -s http://localhost:3000/api/query/ritrovo.upcoming_conferences/execute | jq -r '.items[0].id') | grep -c 'breadcrumb'
echo -n "7. Search API: "; curl -s 'http://localhost:3000/api/search?q=conference' | jq -r '.total'
echo ""
```

Expected output:
```
1. Conference template: > 0
2. CFP template: 200
3. Speaker type: speaker
4. File endpoint: 401
5. Page layout: > 0
6. Breadcrumbs: > 0
7. Search API: > 0
```

Create a database backup:
```bash
$(brew --prefix libpq)/bin/pg_dump \
  postgres://trovato:trovato@localhost:5432/trovato \
  -Fc -f backups/tutorial-part-03-$(date +%Y%m%d).dump
```

Record backup in `TOOLS.md -> Backups`.

All discoveries should be recorded in `TOOLS.md`.
