# Recipe: Part 2 — The Ritrovo Importer Plugin

> **Synced with:** `docs/tutorial/part-02-ritrovo-importer.md`
> **Sync hash:** 824a6d8a
> **Last verified:** 2026-03-07
>
> Run `docs/tutorial/recipes/sync-check.sh` before starting to verify this recipe matches the current tutorial.

---

## Prerequisites

- Part 1 must be completed (conference type exists, three hand-created conferences present).
- Check `TOOLS.md` for server start commands, database connection string, admin URL, and build commands. All of these should have been recorded during Part 1.

---

## 2.1 The WASM Plugin Model

This section is primarily educational (understanding plugins, taps, WASM sandboxing). The key action items are:

### 2.1.1 Understand Plugin Structure

`[REFERENCE]` No action needed. Read the tutorial section to understand:
- Plugins are WASM modules in `plugins/{name}/`
- Taps are exported functions the kernel calls at lifecycle points
- Taps are declared in `{name}.info.toml`
- The `ritrovo_importer` plugin already ships with Trovato

### 2.1.2 Build the WASM Binary

`[CLI]` Check `TOOLS.md -> Plugins` for the WASM build command. If not recorded:

```
cargo build --target wasm32-wasip1 -p ritrovo_importer --release
```

**Verify:** File exists at `target/wasm32-wasip1/release/ritrovo_importer.wasm`.

If the `wasm32-wasip1` target is not installed:
```
rustup target add wasm32-wasip1
```

Record the working build command in `TOOLS.md -> Plugins`.

### 2.1.3 Import Configuration

`[CLI]` Import the tutorial configuration (taxonomy, gather queries, URL aliases) BEFORE installing the plugin:

```
cargo run --release --bin trovato -- config import docs/tutorial/config
```

**Verify:** Output shows `Imported 42 config entities` including 1 category, 32 tags, 5 gather queries, 2 URL aliases.

`[CLI]` Confirm taxonomy and gathers are in the database:
```
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT COUNT(*) FROM category_tag WHERE category_id = 'topics';"
# Expect: 32

$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT query_id FROM gather_query WHERE plugin = 'ritrovo_importer' ORDER BY query_id;"
# Expect: 5 rows
```

### 2.1.4 Install the Plugin

`[CLI]` Check `TOOLS.md -> Plugins` for the install command. If not recorded:

```
cargo run --release --bin trovato -- plugin install ritrovo_importer
```

**Note:** The plugin has `default_enabled = false`, so it was not auto-enabled during Part 1. The `plugin install` command enables it, runs its migrations (creating the `ritrovo_state` table), and marks it ready for `tap_install`.

Record the install command in `TOOLS.md -> Plugins`.

### 2.1.5 Restart the Server and Verify tap_install

`[CLI]` Restart the server using the command from `TOOLS.md -> Server`.

**Verify:** Check server logs for:
```
INFO trovato::state: tap_install dispatched plugin="ritrovo_importer"
```

and:
```
discover_taxonomy_uuids: 23/23 terms found
```

`[CLI]` Verify taxonomy discovery (used by the import pipeline's queue worker — browse routes use `category_tag.slug` instead):
```
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT COUNT(*) FROM ritrovo_state;"
```

**Verify:** Returns > 0 (one row per discovered taxonomy term, plus ETag entries).

**Note:** `tap_install` fires only once per server lifetime. To re-run it (e.g. after DB reset), delete the plugin's row from `plugin_status` and restart.

---

## 2.2 Cron-Driven Conference Import

This section covers the import pipeline architecture. The plugin code is already written — the agent's job is to ensure it runs correctly.

### 2.2.1 Understand the Architecture

`[REFERENCE]` No action needed. Key concepts:
- **Cron phase:** `tap_cron` runs every ~1 minute, processes 5 topics per cycle in round-robin, covers current year and next year
- **24-hour gate:** `should_import` only returns true once per 24 hours
- **ETags:** Conditional HTTP fetches avoid re-downloading unchanged files
- **Queue:** Work is pushed to `ritrovo_import` queue with concurrency 4
- **Worker:** `tap_queue_worker` validates, deduplicates (via `source_id`), and inserts/updates conferences

### 2.2.2 Trigger the Initial Import

`[CLI]` The initial import happens via `tap_install`, which pushes historical data (2015 to current year) onto the queue. After the server has been running for a few minutes, check import progress:

```
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT COUNT(*) FROM item WHERE type = 'conference';"
```

**Verify:** Count should be significantly greater than 3 (the hand-created conferences). The tutorial expects ~5,492 conferences after a full import.

If the count is still 3, the queue workers haven't fired yet. Queue items are processed during cron runs. Trigger cron manually:

```
curl -s -X POST http://localhost:3000/cron/default-cron-key | jq '.status'
# Expect: "completed"
```

One cron run may not drain the full queue. Run it multiple times until the conference count stabilizes (queue depth can be checked with `docker exec trovato-redis-1 redis-cli LLEN queue:ritrovo_import`). The queue is fully drained when it returns 0.

### 2.2.3 Verify Deduplication

`[CLI]` Check that the three hand-created conferences from Part 1 were not duplicated. The importer uses `source_id` for dedup, but hand-created conferences don't have `field_source_id`, so they won't conflict. Verify no duplicate source IDs exist:

```
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT fields->>'field_source_id' AS sid, COUNT(*) FROM item WHERE type = 'conference' AND fields->>'field_source_id' IS NOT NULL AND fields->>'field_source_id' != '' GROUP BY fields->>'field_source_id' HAVING COUNT(*) > 1;"
```

**Verify:** Zero rows returned (no duplicate non-empty source IDs).

### 2.2.4 Verify Field Mapping

`[CLI]` Spot-check an imported conference:

```
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT title, fields->>'field_source_id' AS source_id, fields->>'field_start_date' AS start_date, fields->>'field_city' AS city FROM item WHERE type = 'conference' AND fields->>'field_source_id' IS NOT NULL ORDER BY created DESC LIMIT 5;"
```

**Verify:** Rows have populated `source_id`, `start_date`, and (for non-online events) `city` values.

### 2.2.5 Run Plugin Tests

`[CLI]` Run the ritrovo_importer tests (native build, not WASM):

```
cargo test -p ritrovo_importer
```

**Verify:** All tests pass. The tests cover:
- `tap_queue_info` returns correct queue config
- `tap_cron` runs with stub host functions
- `tap_queue_worker` rejects malformed payloads
- `validate_conference` enforces all validation rules
- `compute_source_id` produces correct slugified IDs
- `build_source_fields` maps fields correctly

---

## 2.3 Hierarchical Topic Taxonomy

### 2.3.1 Understand the Category System

`[REFERENCE]` No action needed. Key concepts:
- `category` table = named vocabulary (e.g. "Conference Topics")
- `category_tag` table = individual terms (e.g. "Rust")
- `category_tag_hierarchy` table = parent-child edges
- Items reference tags by UUID in `field_topics` array
- `HasTagOrDescendants` filter expands UUIDs via recursive CTE

### 2.3.2 Verify Taxonomy Was Imported

`[CLI]` Confirm the topic vocabulary and terms exist:

```
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT label FROM category WHERE id = 'topics';"
```

**Verify:** Returns "Conference Topics" (or "Topics" if the category existed before the plugin ran — this is benign).

```
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT ct.label, ct.slug, cth.parent_id IS NOT NULL AS has_parent FROM category_tag ct LEFT JOIN category_tag_hierarchy cth ON ct.id = cth.tag_id WHERE ct.category_id = 'topics' ORDER BY ct.weight, ct.label;"
```

**Verify:** Returns multiple rows with topic labels (Languages, Infrastructure, AI & Data, etc.), slugs (e.g. `rust`, `java`, `ai-data`), and hierarchy info. Every tag should have a non-null `slug` value — these are used by gather route aliases to resolve `/topics/{slug}` URLs.

### 2.3.3 Verify Topic UUIDs in Imported Conferences

`[CLI]`
```
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT title, fields->'field_topics' AS topics FROM item WHERE type = 'conference' AND fields->'field_topics' IS NOT NULL AND jsonb_array_length(fields->'field_topics') > 0 LIMIT 5;"
```

**Verify:** Conferences have `field_topics` arrays containing UUID strings (not raw slug strings like "rust").

### 2.3.4 Verify the /topics/{slug} Gather Route Alias

`[CLI]` The `/topics/{slug}` route is a **gather route alias** — declared in the `ritrovo.by_topic` query's `display.routes` config, not hard-coded. It resolves slugs to tag UUIDs via the `category_tag.slug` column.

```
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/topics/rust
```

**Verify:** Returns `307` (temporary redirect to the gather query with topic UUID).

```
curl -sL -o /dev/null -w "%{http_code}" http://localhost:3000/topics/rust
```

**Verify:** Returns `200` (following the redirect to the gather results).

```
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/topics/nonexistent-slug
```

**Verify:** Returns `404`.

---

## 2.4 Advanced Gathers

### 2.4.1 Verify the Five Gather Queries

`[CLI]` Confirm all five gather queries were imported via config:

```
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT query_id, label FROM gather_query WHERE plugin = 'ritrovo_importer' ORDER BY query_id;"
```

**Verify:** Five rows:
- `ritrovo.by_city` — Conferences by City
- `ritrovo.by_country` — Conferences by Country
- `ritrovo.by_topic` — Conferences by Topic
- `ritrovo.open_cfps` — Open CFPs
- `ritrovo.upcoming_conferences` — Upcoming Conferences

### 2.4.2 Verify ritrovo.upcoming_conferences

`[CLI]`
```
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/conferences
```

**Verify:** Returns `200`. This now serves the full ritrovo.upcoming_conferences gather (upgraded from the Part 1 alias).

`[CLI]` Verify the API returns results:
```
curl http://localhost:3000/api/query/ritrovo.upcoming_conferences/execute | jq '.total'
```

**Verify:** Returns a positive number (upcoming conferences from the imported dataset).

`[CLI]` Verify the page renders conference cards with filter widgets:

```
# Conference cards are present
curl -s http://localhost:3000/conferences | grep -c 'class="conf-card__title"'
# Expect: 20 (default page size)

# Exposed filter widgets are present (topic, country, online, language)
curl -s http://localhost:3000/conferences | grep -oE 'name="fields\.[a-z_]+"' | sort -u
# Expect:
#   name="fields.field_country"
#   name="fields.field_language"
#   name="fields.field_online"
#   name="fields.field_topics"
```

### 2.4.3 Verify ritrovo.open_cfps

`[CLI]`
```
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/cfps
```

**Verify:** Returns `200`.

### 2.4.4 Verify Location Gather Route Aliases

`[CLI]` Location routes are **gather route aliases** declared in the `ritrovo.by_country` and `ritrovo.by_city` query YAML configs using pass-through parameter mapping.

First, find a country that has conferences:
```
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT DISTINCT fields->>'field_country' AS country FROM item WHERE type = 'conference' AND fields->>'field_country' IS NOT NULL LIMIT 5;"
```

Then test the location route with one of the returned countries (URL-encode if needed):
```
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/location/Germany
```

**Verify:** Returns `307` (temporary redirect to the by_country gather).

```
curl -sL -o /dev/null -w "%{http_code}" http://localhost:3000/location/Germany
```

**Verify:** Returns `200` (following redirect).

`[CLI]` Test the two-segment city route (find a city from the dataset first):
```
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT fields->>'field_country' AS country, fields->>'field_city' AS city FROM item WHERE type = 'conference' AND fields->>'field_city' IS NOT NULL AND fields->>'field_city' != '' LIMIT 1;"
```

Then test with the returned country/city:
```
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/location/Germany/Berlin
```

**Verify:** Returns `307` (temporary redirect to the by_city gather).

### 2.4.5 Verify Exposed Filter Widgets

`[UI-ONLY]` Visit `http://localhost:3000/conferences` in a browser. Confirm the filter form includes:
- A **topic selector** (hierarchical dropdown with indented child terms)
- A **country** dropdown or autocomplete
- An **Online Only** selector (Any / Yes / No)
- A **language** dropdown or autocomplete

Test each filter by selecting a value and submitting. Verify the URL stays on `/conferences` (not `/gather/ritrovo.upcoming_conferences`).

### 2.4.6 Verify 301 Redirects for Raw Gather URLs

`[CLI]`
```
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/gather/ritrovo.upcoming_conferences
```

**Verify:** Returns `301` (redirect to `/conferences`).

```
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/gather/ritrovo.open_cfps
```

**Verify:** Returns `301` (redirect to `/cfps`).

---

## Completion Checklist

After completing all steps, verify the full Part 2 outcome:

- [ ] Config imported (42 entities: 1 category, 32 tags, 5 gather queries, 2 URL aliases)
- [ ] `ritrovo_importer` plugin is installed and `tap_install` has run
- [ ] WASM binary builds successfully
- [ ] Plugin tests pass (`cargo test -p ritrovo_importer`)
- [ ] ~5,492 conferences imported (count may vary with dataset updates)
- [ ] No duplicate `source_id` values
- [ ] Topic taxonomy imported with hierarchical terms (Languages > Systems > Rust, etc.) and slugs
- [ ] `field_topics` contains UUID arrays, not raw slugs
- [ ] `/topics/rust` returns 307 redirect to gather (via gather route alias + `category_tag.slug`)
- [ ] Five gather queries exist with `plugin = 'ritrovo_importer'`
- [ ] `/conferences` serves the full upcoming_conferences gather with exposed filter widgets
- [ ] `/cfps` serves open CFPs listing
- [ ] `/location/{country}` and `/location/{country}/{city}` routes work
- [ ] Raw gather URLs (`/gather/ritrovo.upcoming_conferences`, `/gather/ritrovo.open_cfps`) redirect 301 to canonical URLs
- [ ] All discoveries recorded in `TOOLS.md`
