# Part 2: The Ritrovo Importer Plugin

In Part 1, you built a Trovato site and manually created three conferences. That works for demos, but Ritrovo needs hundreds of real conferences — pulled automatically from the open-source [confs.tech](https://github.com/tech-conferences/conference-data) dataset.

In this part you'll build the `ritrovo_importer` plugin: a WASM module that runs on a daily cron cycle, fetches conference JSON from GitHub, and keeps your database up to date. Along the way you'll learn how Trovato's plugin system works and when to reach for it.

---

## 2.1 The WASM Plugin Model

### What is a plugin?

A Trovato plugin is a WebAssembly module (`.wasm` file) that the kernel discovers at startup, loads into an isolated sandbox, and calls at specific lifecycle points called **taps**.

Plugins live in `plugins/{name}/` as Rust `cdylib` crates. They compile to WASM and are discovered automatically — drop a `.wasm` next to an `.info.toml` manifest and the server picks it up.

### Why WASM?

The WASM sandbox enforces hard limits:

| Resource | Limit |
|---|---|
| Database query timeout | 5 s |
| HTTP request timeout | 30 s |
| Clock ticks (epoch interruption) | 10 ticks |

If a plugin hangs, the kernel kills it without affecting the rest of the site. This is the same reason browsers run untrusted JavaScript in a sandbox — isolation is the point.

Plugins cannot access the filesystem, spawn threads, or open sockets directly. All I/O goes through **host functions**: `db_query`, `http_request`, `queue_push`, `log`, and a handful of others. The kernel controls what plugins can do.

### Taps

A tap is a function your plugin exports that the kernel calls at a specific moment. Think of them like webhooks, but in-process and sandboxed.

```
Kernel event → serialise inputs to JSON → call plugin tap → deserialise JSON result
```

You declare which taps you implement in `{name}.info.toml`:

```toml
[taps]
implements = ["tap_install", "tap_cron", "tap_perm", "tap_menu"]
```

In Rust, each tap is a regular function annotated with `#[plugin_tap]`. The macro generates the WASM export boilerplate — reading JSON from WASM memory, calling your function, writing the result back:

```rust
#[plugin_tap]
pub fn tap_cron(input: CronInput) -> serde_json::Value {
    // your logic here
    serde_json::json!({ "status": "ok" })
}
```

### Scaffolding a new plugin

The `trovato plugin new` command generates the boilerplate for you:

```bash
cargo run --release --bin trovato -- plugin new my_plugin
```

This creates:

```
plugins/my_plugin/
  Cargo.toml               # cdylib crate, trovato-sdk dependency
  my_plugin.info.toml      # manifest: name, version, taps
  src/lib.rs               # stub implementations for 4 taps
  migrations/              # empty, ready for SQL migration files
```

It also adds `"plugins/my_plugin"` to the workspace `Cargo.toml` members list.

> **Note:** The `ritrovo_importer` plugin already ships with Trovato as a complete example. You don't need to scaffold it — instead, read through its source to understand the patterns, then use `cargo run --release --bin trovato -- plugin install ritrovo_importer` to enable it.

### Installing and enabling a plugin

After building the `.wasm`:

```bash
cargo build --target wasm32-wasip1 -p ritrovo_importer --release
cargo run --release --bin trovato -- plugin install ritrovo_importer
```

`plugin install`:
1. Copies `target/wasm32-wasip1/release/{name}.wasm` into the plugin directory (where the server can find it).
2. Runs any pending SQL migrations.
3. Marks the plugin as enabled in `plugin_status`.

> **Note:** Because `ritrovo_importer` has `default_enabled = true` in its `info.toml`, the server auto-installs it into `plugin_status` on first startup — so you may see the message `Plugin 'ritrovo_importer' is already installed` and that is expected. The command still ensures any pending migrations are applied.

### Import Configuration Before Starting

Before starting the server, import the tutorial configuration files. These YAML files define the topic taxonomy, gather queries, and URL aliases that the importer plugin depends on:

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config
```

This imports:
- **1 category** — the "Conference Topics" vocabulary
- **32 tags** — hierarchical topic terms (Languages > Systems > Rust, etc.)
- **5 gather queries** — the browse pages (upcoming conferences, open CFPs, by topic/country/city)
- **2 URL aliases** — `/conferences` and `/cfps` pointing to their gather queries

> **Why config, not plugin?** Taxonomies, gather queries, and URL aliases are core Trovato concepts with first-class support in the config import/export system. The plugin only handles what the kernel can't: fetching external data from the confs.tech API and mapping it to conference items.

### First Startup: tap_install

When the server next starts with the plugin enabled, it calls `tap_install` **once** for any plugin where it has not previously run. You'll see this in the server logs:

```
INFO trovato::state: tap_install dispatched plugin="ritrovo_importer"
```

`tap_install` discovers the taxonomy term UUIDs from the config-imported data (caching them in `ritrovo_state` for the import pipeline's queue worker — browse routes use `category_tag.slug` instead) and queues the full historical conference import. After a restart you can verify it ran:

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT COUNT(*) FROM ritrovo_state;"
# Should return > 0 (one row per discovered taxonomy term, plus ETag entries)
```

> **Note:** `tap_install` fires only once per server lifetime — subsequent restarts skip it for already-installed plugins. To re-run it (e.g. after resetting the database), delete the plugin's row from `plugin_status` and restart.

### The four stubs: tap_install, tap_cron, tap_queue_info, tap_queue_worker

The generated scaffold includes stubs for the four taps the importer uses:

| Tap | When called | What it does |
|---|---|---|
| `tap_install` | Once, on first enable | Discovers taxonomy UUIDs, queues historical import |
| `tap_cron` | Every cron cycle (~1 min) | Fetches conference data, pushes to queue |
| `tap_queue_info` | At startup | Declares queue names and concurrency |
| `tap_queue_worker` | Per queue job | Validates and inserts/updates conferences |

The next section covers how `tap_cron` and the queue work together.

> **Note:** The `ritrovo_importer` exemplar plugin declares only `tap_install`, `tap_cron`, `tap_perm`, and `tap_menu` — not the queue taps. The queue infrastructure is introduced in section 2.2. The scaffold includes all four tap stubs upfront so you don't have to add them later.

---

## 2.2 Cron-Driven Conference Import

The importer follows a two-phase architecture: a **cron phase** that decides what to fetch and enqueues the work, and a **worker phase** that validates and persists each batch. The two phases are decoupled by a database-backed queue, which lets the kernel run up to four import batches in parallel without blocking the cron dispatcher.

```
tap_cron (every 24 h)
  └── for each topic+year
        ├── GET /conferences/{year}/{topic}.json (If-None-Match: <etag>)
        │     ├── 304 Not Modified → skip
        │     └── 200 OK → store new ETag → queue_push("ritrovo_import", payload)
        │
kernel queue drain
  └── tap_queue_worker (×N, concurrency 4)
        ├── parse payload
        ├── validate each entry
        └── INSERT or UPDATE conference item
```

### Conditional HTTP: ETags

Every JSON file served from the confs.tech GitHub repo carries an `ETag` response header. An ETag is an opaque string that identifies a specific version of the file — when the file changes, the ETag changes.

After fetching a file successfully, the importer stores its ETag in the `ritrovo_state` table under a key like `etag.rust.2026`. On the next cron run, it sends that ETag back in the `If-None-Match` request header. If the file has not changed, GitHub responds with `304 Not Modified` (no body), and the importer skips that topic+year — no queue job, no DB write.

This makes the daily import cheap: on a typical day, only a handful of files change.

```rust
// Build the request, attaching the stored ETag if we have one.
let mut request = HttpRequest::get(&url).timeout(15_000);
if let Some(ref etag) = stored_etag {
    request = request.header("If-None-Match", etag);
}

match response.status {
    304 => FetchResult::NotModified,   // same file — skip
    200 => {
        // Store the new ETag for next time.
        if let Some(etag) = response.headers.get("etag") {
            set_state(&etag_key(topic, year), etag);
        }
        // Push work onto the queue.
        host::queue_push(QUEUE_NAME, &payload)?;
        FetchResult::Queued
    }
    // ...
}
```

ETag state is namespaced: `etag.{topic}.{year}`. The importer tracks ETags per file independently, so a change to `2026/rust.json` does not force a re-import of `2026/javascript.json`.

### Round-Robin Topic Scheduling

There are 25 topics in the confs.tech dataset. Fetching all of them every minute would be wasteful and risk hitting GitHub's rate limits. Instead, `tap_cron` processes five topics per cycle in a round-robin:

```rust
const TOPICS_PER_CYCLE: usize = 5;       // topics per cron run
const IMPORT_INTERVAL_SECS: i64 = 86_400; // skip runs inside a 24-hour window

let topic_offset = load_state_usize(STATE_TOPIC_OFFSET, TOPICS.len());
let cycle_topics: Vec<&str> = TOPICS
    .iter()
    .cycle()
    .skip(topic_offset)
    .take(TOPICS_PER_CYCLE)
    .copied()
    .collect();

// Advance offset for next run.
let next_offset = (topic_offset + TOPICS_PER_CYCLE) % TOPICS.len();
save_state(STATE_TOPIC_OFFSET, &next_offset.to_string());
```

The offset persists across restarts in the `ritrovo_state` table. Each cron cycle also covers two years — `current_year` and `current_year + 1` — so a cycle pushes at most 10 queue jobs.

The outer 24-hour gate (`should_import`) means the round-robin only advances once per day. The cron scheduler fires every minute, but `should_import` returns false until 24 hours have elapsed since `STATE_LAST_IMPORT`.

### Declaring the Queue: tap_queue_info

Before the kernel can drain the queue, it needs to know which queues exist and how many workers to run in parallel. `tap_queue_info` declares this at startup:

```rust
#[plugin_tap]
pub fn tap_queue_info() -> serde_json::Value {
    serde_json::json!([
        { "name": "ritrovo_import", "concurrency": 4 }
    ])
}
```

The kernel reads this once at startup (and whenever it reloads plugins). A `concurrency` of `4` means the kernel may dispatch up to four `tap_queue_worker` calls simultaneously for this queue. For a write-heavy importer this is a good balance: parallel enough to drain the queue quickly, conservative enough not to hammer the database.

A plugin can declare multiple queues by returning an array with more than one entry. Each entry may have a different concurrency.

### Processing a Batch: tap_queue_worker

The kernel calls `tap_queue_worker` once per item in the `ritrovo_import` queue. Each item's payload contains a topic name, a year, and the raw JSON body of the confs.tech file:

```json
{
    "topic": "rust",
    "year": 2026,
    "conferences": "[{\"name\":\"RustConf\",...}, ...]"
}
```

The worker:

1. Validates the payload shape (topic, year, conferences are all present).
2. Parses the `conferences` string as JSON into a `Vec<ConfsTechEntry>`.
3. Validates each entry (see "Validation rules" below).
4. For each valid entry, computes a `source_id`, checks whether a matching conference already exists, then inserts or updates.

```rust
#[plugin_tap]
pub fn tap_queue_worker(input: serde_json::Value) -> serde_json::Value {
    // 1. Extract required fields from the payload.
    let topic = input["topic"].as_str()...;
    let year  = input["year"].as_u64()...;
    let body  = input["conferences"].as_str()...;

    // 2. Parse the conference list.
    let conferences: Vec<ConfsTechEntry> = serde_json::from_str(&body)?;

    // 3. Process each entry.
    for conf in &conferences {
        match validate_conference(conf) {
            Err(reason) => { log_warning(...); invalid += 1; continue; }
            Ok(()) => {}
        }
        let source_id = compute_source_id(conf);
        if let Some(info) = existing.get(&source_id) {
            // Conference exists — update it.
        } else {
            // New conference — insert it.
        }
    }
    // ...
}
```

The worker returns a summary JSON object so the kernel can log outcomes:

```json
{
    "status": "ok",
    "topic": "rust",
    "year": 2026,
    "imported": 12,
    "updated": 3,
    "skipped": 41,
    "invalid": 1
}
```

### Validation Rules

The importer rejects entries that would create unusable or nonsensical data. Invalid entries are logged with a human-readable reason and counted as `invalid` in the worker's summary — they are never silently dropped.

| Rule | Condition | Log message example |
|---|---|---|
| Name required | `name` must be non-empty | `missing required field: name` |
| Start date required | `startDate` must be non-empty | `missing required field: startDate` |
| End date required | `endDate` must be non-empty | `missing required field: endDate` |
| Date format | Must match `YYYY-MM-DD`, year 2010–2035 | `invalid startDate format: '01-09-2026'` |
| Date ordering | `endDate` ≥ `startDate` | `endDate '2026-08-31' is before startDate '2026-09-01'` |
| CFP ordering | `cfpEndDate` ≤ `startDate` (if present) | `cfpEndDate '2026-10-01' is after startDate '2026-09-01'` |

### Deduplication: Source ID

The same conference can appear in multiple topic files (e.g., `rust.json` and `systems.json` may both list RustConf). Without deduplication, each import cycle would create duplicates.

The importer uses a stable `source_id` field to identify conferences across runs and topics:

```
source_id = slugify(name) + "-" + start_date + "-" + slugify(city ?? "online")
```

Examples:

| name | startDate | city | source_id |
|---|---|---|---|
| RustConf | 2026-09-01 | Portland | `rustconf-2026-09-01-portland` |
| Vue.js Nation | 2025-01-29 | *(none)* | `vue-js-nation-2025-01-29-online` |
| C++ Now! | 2026-05-05 | Aspen | `c-now-2026-05-05-aspen` |

Before inserting, the worker loads all existing conference items' `field_source_id` values into a `HashMap`. Lookup is O(1); the entire dedup check is a single DB query per batch rather than one per conference.

When a conference is found by `source_id`, the worker **updates** it using JSONB merge (`fields = fields || $new::jsonb`). This overwrites source-derived fields (dates, URL, city, country) while preserving fields the Ritrovo editor may have added manually, like `field_description` and `field_editor_notes`.

When a conference appears in a new topic file for the first time, the worker merges the topic into the existing `field_topics` array rather than replacing it. RustConf filed under both "rust" and "systems" will have `field_topics: ["rust", "systems"]`.

### Concurrent Worker Safety

The HashMap dedup is sufficient for sequential processing, but the queue runs with `concurrency = 4`. This creates a race window: if four workers each process a different topic file that contains the same conference, they all load the HashMap **before** any insert commits, all see the conference as missing, and all INSERT — producing 3–4 copies with different `field_topics` arrays.

The importer defends against this with two layers:

**Database-level uniqueness.** Migration `002_dedup_conferences.sql` creates a unique partial index:

```sql
CREATE UNIQUE INDEX uniq_item_conference_source_id
    ON item ((fields->>'field_source_id'))
    WHERE type = 'conference'
      AND fields->>'field_source_id' IS NOT NULL
      AND fields->>'field_source_id' != '';
```

This makes it impossible for any two `conference` items to share a `field_source_id`, regardless of which code path inserted them.

**Upsert on insert.** `insert_conference` uses `INSERT … ON CONFLICT DO UPDATE` rather than a plain `INSERT`. When a concurrent worker races past the HashMap check, the conflicting insert becomes an update that merges `field_topics` at the SQL level:

```sql
ON CONFLICT ((fields->>'field_source_id'))
WHERE type = 'conference'
  AND fields->>'field_source_id' IS NOT NULL
  AND fields->>'field_source_id' != ''
DO UPDATE SET
  title   = EXCLUDED.title,
  changed = EXCLUDED.changed,
  fields  = item.fields
         || (EXCLUDED.fields - 'field_topics')
         || jsonb_build_object(
              'field_topics',
              (SELECT COALESCE(jsonb_agg(t ORDER BY t), '[]'::jsonb)
               FROM (
                 SELECT jsonb_array_elements_text(item.fields->'field_topics')
                 UNION
                 SELECT jsonb_array_elements_text(EXCLUDED.fields->'field_topics')
               ) u(t))
            )
```

The `UNION` (not `UNION ALL`) deduplicates topic UUIDs before aggregating, so however many workers race to insert the same conference, the result is always a single row with all the correct topics merged in.

> **Design note:** The HashMap path (`update_conference`) is still the primary hot path — it handles sequential imports and day-to-day cron runs cheaply. The upsert is the safety net that makes concurrent imports correct even when the application-level check races.

### Field Mapping

The confs.tech JSON schema maps to `conference` item fields as follows:

| confs.tech field | item field | notes |
|---|---|---|
| `name` | `title` (item column) | required |
| `url` | `field_url` | omitted if empty |
| `startDate` | `field_start_date` | required; YYYY-MM-DD |
| `endDate` | `field_end_date` | required; YYYY-MM-DD |
| `city` | `field_city` | optional |
| `country` | `field_country` | optional |
| `online` | `field_online` | defaults to `false` |
| `cfpUrl` | `field_cfp_url` | optional |
| `cfpEndDate` | `field_cfp_end_date` | optional |
| `locales` | `field_language` | optional |
| `twitter` | `field_twitter` | optional |
| `cocUrl` | `field_coc_url` | optional |
| *(computed)* | `field_source_id` | see dedup section |
| *(from queue payload)* | `field_topics` | accumulated across topic files |

Newly inserted conferences are created as **published** (`status = 1`) on the live stage, so they appear immediately in the public browse pages without requiring a manual publish step.

### Historical Import: tap_install

When the plugin is first enabled, `tap_install` first discovers taxonomy term UUIDs from the config-imported `category_tag` rows (caching them in `ritrovo_state` for the queue worker), then runs a full historical backfill. It fetches every topic file for every year from 2015 to the current year, stores ETags, and pushes each successful response onto the queue:

```rust
for year in FIRST_IMPORT_YEAR..=current_year {   // 2015..=now
    for topic in TOPICS {
        // GET https://raw.githubusercontent.com/…/{year}/{topic}.json
        // On 200: store ETag, push payload onto queue
        // On 404: skip (not all topics exist for every year)
    }
}
```

The `tap_install` function does not wait for the queue to drain. It exits quickly after pushing all the jobs, and the actual DB writes happen over subsequent cron cycles as the kernel drains the queue at the configured concurrency.

> **Note:** `tap_install` uses `host::query_raw("SELECT EXTRACT(YEAR FROM NOW())::int AS y", &[])` to get the current year. WASM plugins do not have access to the system clock directly — the DB is the authoritative time source.

### trovato-test: Verifying the Import Pipeline

The unit tests in `plugins/ritrovo_importer/src/lib.rs` exercise the plugin logic in a native (non-WASM) build, using stub host functions that return predictable values. Here is what each test category verifies:

**Queue declaration**

```
trovato-test: tap_queue_info
  - returns an array with exactly one queue entry
  - queue name is "ritrovo_import"
  - concurrency is 4
```

**Cron fires and queues work**

```
trovato-test: tap_cron
  - given no previous import timestamp (stub query_raw returns "[]")
  - when tap_cron runs with a timestamp
  - then status == "completed"
  - and at least 0 errors (stub http_request returns 200 with body "[]")
```

**Worker rejects malformed payloads**

```
trovato-test: tap_queue_worker — payload validation
  - missing "topic" field  → status "error", reason "missing_topic"
  - missing "year" field   → status "error", reason "missing_year"
  - "conferences" is not JSON → status "error", reason "parse_error"
```

**Worker skips invalid conference entries**

```
trovato-test: tap_queue_worker — entry validation
  - payload contains one entry with name but no startDate/endDate
  - status == "ok", invalid == 1, imported == 0
```

**Validation rules**

```
trovato-test: validate_conference
  - valid entry with name, startDate "2026-09-01", endDate "2026-09-03" → Ok
  - empty name       → Err("missing required field: name")
  - empty startDate  → Err("missing required field: startDate")
  - endDate before startDate (e.g. 2026-08-31 < 2026-09-01) → Err
  - cfpEndDate after startDate (e.g. 2026-10-01 > 2026-09-01) → Err
  - startDate "01-09-2026" (wrong format) → Err
  - startDate "1999-01-01" (year out of range) → Err
```

**Deduplication**

```
trovato-test: compute_source_id
  - name "RustConf", startDate "2026-09-01", city "Portland"
    → "rustconf-2026-09-01-portland"
  - name "Vue.js Nation", startDate "2025-01-29", no city
    → "vue-js-nation-2025-01-29-online"
```

**Field mapping**

```
trovato-test: build_source_fields
  - minimal entry (no optional fields)
    → field_source_id present, field_online == false, no field_url key
  - full entry with all optional fields
    → field_url, field_city, field_country, field_cfp_url, field_language all present
```

---

## 2.3 Hierarchical Topic Taxonomy

The importer stores each conference's topic as a `field_topics` array. In section 2.2, that array held raw confs.tech slug strings like `"rust"` and `"javascript"`. In this section, you'll replace those strings with **category tag UUIDs** from a proper taxonomy — which unlocks hierarchical filtering ("show me all Languages conferences") via the `HasTagOrDescendants` gather operator.

### The Category System

Trovato's category system organises tags into named vocabularies.

| Table | What it holds |
|---|---|
| `category` | A named vocabulary (e.g. "Conference Topics") |
| `category_tag` | A single term inside a vocabulary (e.g. "Rust") |
| `category_tag_hierarchy` | Parent→child edges (allows multi-level trees) |

A category tag is identified by a UUID generated at insert time. Items reference tags by UUID in their JSONB `fields`, typically as an array: `field_topics: ["<uuid1>", "<uuid2>"]`. The `HasTagOrDescendants` filter understands these UUIDs and their parent-child relationships.

### Defining the Taxonomy in Configuration

The topic taxonomy is defined as YAML configuration files in `docs/tutorial/config/`. Each term is a separate file (e.g. `tag.{uuid}.yml`) with its label, category, slug, weight, and parent references:

```yaml
# tag.8835efe1-04a9-4dc4-bc4b-7d47ec387031.yml
id: 8835efe1-04a9-4dc4-bc4b-7d47ec387031
category_id: topics
label: Ruby
slug: ruby
weight: 4
parents:
- f95ddda0-58d4-4519-9965-91c86d74a459  # Web Languages
```

The `slug` field is a URL-safe identifier used by **gather route aliases** (see "Gather Route Aliases" below) to resolve friendly URLs like `/topics/ruby` to the tag's UUID. Slugs are unique within a category and stored in the `category_tag.slug` column.

The category itself is also a YAML file:

```yaml
# category.topics.yml
id: topics
label: Conference Topics
description: Hierarchical topic taxonomy for tech conferences
hierarchy: 2
weight: 0
```

These files are imported via `cargo run --release --bin trovato -- config import docs/tutorial/config` before the plugin is installed. The config import system handles dependency ordering (categories before tags) and uses `ON CONFLICT DO UPDATE` for idempotency.

Some confs.tech slugs (like `sre` and `scala`) are intentionally absent from the taxonomy. Conferences with those topics simply have an empty `field_topics` array — they are still imported, just not reachable via topic browse pages.

### Discovering Term UUIDs at Install Time

When `tap_install` runs, it calls `discover_taxonomy_uuids()`, which looks up each term's UUID by label from the `category_tag` table and caches it in `ritrovo_state`. This cache is used by the **import pipeline** (queue worker) to map confs.tech topic slugs to tag UUIDs when inserting conferences — it is separate from the browse route slug resolution, which uses the `category_tag.slug` column directly:

```rust
fn discover_taxonomy_uuids() -> u32 {
    let mut discovered = 0u32;

    for &(_confs_slug, term_slug, term_label) in SLUG_TO_TERM {
        let state_key = format!("{STATE_TOPIC_TERM_PREFIX}.{term_slug}");

        if load_state_str(&state_key).is_some() {
            discovered += 1;
            continue;
        }

        let result = host::query_raw(
            "SELECT id::text AS id FROM category_tag \
             WHERE category_id = $1 AND label = $2 LIMIT 1",
            &[
                serde_json::json!(TOPICS_CATEGORY_ID),
                serde_json::json!(term_label),
            ],
        );
        if let Some(uuid) = /* parse result */ {
            save_state(&state_key, &uuid);
            discovered += 1;
        }
    }
    discovered
}
```

The `SLUG_TO_TERM` constant maps confs.tech topic slugs to taxonomy term slugs and labels. This is the only taxonomy-related data compiled into the plugin — the actual category and tag definitions live in config YAML.

### Storing Tag UUIDs in field_topics

The queue worker now resolves each topic slug to its UUID before inserting or updating:

```rust
pub fn tap_queue_worker(input: serde_json::Value) -> serde_json::Value {
    let topic = input["topic"].as_str()...;

    // Look up UUID — returns None for unmapped slugs like "sre".
    let topic_uuid = topic_term_uuid(topic);

    // Update or insert, passing the Option<&str> through.
    let merged = merge_topics(&info.topics, topic_uuid.as_deref());
    // ...
}
```

`topic_term_uuid(slug)` checks the `ritrovo_state` table for `topic_term.{slug}`. It returns `None` for any slug not present in `TAXONOMY`.

`merge_topics(existing, new_uuid: Option<&str>)` adds the UUID to the existing array only if it isn't already there, then sorts. When `new_uuid` is `None` (unmapped slug), the existing array is returned unchanged:

```rust
fn merge_topics(existing: &[String], new_uuid: Option<&str>) -> Vec<String> {
    let mut topics: Vec<String> = existing.to_vec();
    if let Some(uuid) = new_uuid && !topics.iter().any(|t| t == uuid) {
        topics.push(uuid.to_string());
    }
    topics.sort();
    topics
}
```

This means conferences tagged with both `rust` and `systems` by two separate confs.tech files accumulate both UUIDs — and a query for "Languages" (the grandparent of both) will still find them.

### The HasTagOrDescendants Operator

Trovato's gather engine supports a `has_tag_or_descendants` filter operator. When the gather service encounters this operator, it first expands the given tag UUID into the full set of descendant UUIDs using a recursive CTE:

```sql
WITH RECURSIVE descendants AS (
    SELECT tag_id FROM category_tag_hierarchy WHERE parent_id = $1
    UNION ALL
    SELECT h.tag_id FROM category_tag_hierarchy h
    JOIN descendants d ON h.parent_id = d.tag_id
)
SELECT tag_id FROM descendants
UNION ALL
SELECT $1::uuid  -- include the tag itself
```

The filter is then rewritten to `HasAnyTag` with the full expanded list. A query for "Languages" (`uuid = xxxxxxxx-…`) automatically matches conferences tagged with "Systems", "Rust", "C++", ".NET", "JVM", "Java", "JavaScript", "Python", "Go", and any other term descending from "Languages".

> **Null handling:** When a `has_tag_or_descendants` filter has a null value — as happens when an exposed filter has no user input — the gather service skips the filter entirely rather than erroring. This lets the same gather definition work both as an exposed filter form (no value → show all) and as a driven query (value provided → filter by topic).

### Gather Route Aliases

The kernel's `gather_routes` module provides a generic mechanism for mapping friendly URLs to gather queries. Rather than hard-coding routes for each plugin, route aliases are declared in the gather query's `display.routes` YAML configuration and registered dynamically at startup.

Each route alias defines a URL path pattern and a list of parameter mappings. Two mapping modes are supported:

| Mode | Config | Behaviour |
|---|---|---|
| **Pass-through** | `segment` + `param` | Path segment value is passed directly as a gather query parameter |
| **Tag slug lookup** | `segment` + `param` + `tag_category` | Path segment is resolved to a tag UUID via `category_tag.slug`, then the UUID is passed |

For the `/topics/{slug}` route, the `ritrovo.by_topic` gather query declares:

```yaml
# gather_query.ritrovo.by_topic.yml (display section)
display:
  routes:
  - path: /topics/{slug}
    params:
    - segment: slug
      param: topic
      tag_category: topics
```

When a request arrives at `/topics/rust`, the handler:

1. Validates the slug (non-empty, ≤ 128 chars, alphanumeric + `-`/`_` only).
2. Looks up the tag by slug in the `category_tag` table: `SELECT ... WHERE category_id = 'topics' AND slug = 'rust'`.
3. Returns 404 if the slug is unknown.
4. Redirects (307 Temporary) to `/gather/ritrovo.by_topic?topic=<uuid>`, preserving any extra query parameters (e.g. `?page=2`).

The tag slug column (`category_tag.slug`, added by the `20260307000001_add_category_tag_slug` kernel migration) is what makes this generic — any category's tags can have slugs, so any gather query can use slug-based route aliases without plugin-specific code. Slugs are unique per category via a partial unique index.

The redirect approach means the browse route reuses the gather engine's full HTML rendering pipeline — no duplication of display logic.

> **URL design choice:** `/topics/rust` redirects to `/gather/ritrovo.by_topic?topic=<uuid>`. The gather URL is bookmarkable and works independently of the slug route. Deep-linking into a specific page of results (`/gather/ritrovo.by_topic?topic=<uuid>&page=3`) works without going through the slug route again.

---

## 2.4 Advanced Gathers

Trovato's gather system converts a declarative JSON query definition into a paginated, optionally-filtered SQL query. In this section you'll see how the importer seeds its five gather queries, and how two kinds of dynamic filter values — contextual values and exposed filters — allow a single query definition to serve both public browse pages and editor search forms.

### What a Gather Query Looks Like

A gather query is stored in the `gather_query` table with two JSONB columns:

- **`definition`** — the query: filters, sorts, joins, item type
- **`display`** — rendering configuration: format (table/card/tile), items per page, pager style, empty text

Here is a complete example:

```json
{
    "definition": {
        "base_table": "item",
        "item_type": "conference",
        "filters": [
            {
                "field": "fields.field_start_date",
                "operator": "greater_or_equal",
                "value": "current_date"
            },
            {
                "field": "fields.field_country",
                "operator": "equals",
                "value": null,
                "exposed": true,
                "exposed_label": "Country"
            }
        ],
        "sorts": [{ "field": "fields.field_start_date", "direction": "asc" }],
        "stage_aware": true
    },
    "display": {
        "format": "table",
        "items_per_page": 20,
        "pager": { "enabled": true, "style": "full", "show_count": true },
        "empty_text": "No upcoming conferences found."
    }
}
```

### Filter Value Types

Gather filters support three kinds of values:

| Kind | JSON representation | When resolved |
|---|---|---|
| **Literal** | `"Germany"`, `true`, `"2026-01-01"` | Compiled into the SQL — fixed for all requests |
| **Contextual** | `"current_date"` or `{"url_arg": "country"}` | Resolved at request time from context |
| **Null** | `null` | Skipped (filter omitted from SQL) unless the user submits a value via an exposed filter form |

**`current_date`** resolves to today's date string (`"YYYY-MM-DD"`) at the moment of each request. This makes it possible to write "conferences starting on or after today" without a literal date.

**`{"url_arg": "name"}`** reads the named key from the gather URL's query string. For example, `value: {"url_arg": "topic"}` takes its value from the `?topic=…` parameter. If the parameter is absent, the filter resolves to null and is skipped — the query returns unfiltered results.

**`null`** on an unexposed filter is meaningless (the filter would always be skipped). On an **exposed** filter it acts as "no default value" — the user can supply one through the rendered filter form, but the gather renders all results when the form is empty.

### The Five Gather Queries

These five queries are defined as YAML configuration files in `docs/tutorial/config/` and imported via `config import`. Each file contains the query ID, label, definition (filters, sorts), and display configuration (format, pager, empty text). Re-importing updates existing queries via `ON CONFLICT DO UPDATE`, so changes propagate automatically.

#### ritrovo.upcoming_conferences

Upcoming conferences (start date ≥ today), filterable via an exposed filter form.

```
Hard filters:    field_start_date ≥ current_date
Exposed filters: field_topics   has_tag_or_descendants  widget: taxonomy_select (vocabulary: "topics")
                 field_country  equals                  widget: dynamic_options (source: fields.field_country)
                 field_online   equals                  widget: boolean
                 field_language equals                  widget: dynamic_options (source: fields.field_language)
Sort:            field_start_date ASC
```

This is the main public listing page, reachable at `/conferences`. The query's display config sets `"canonical_url": "/conferences"`, so all pager links and filter form actions stay on that path rather than the raw `/gather/ritrovo.upcoming_conferences` URL. When all exposed filters are empty (no user input), every filter resolves to null and is skipped — the query returns every upcoming conference. See "Exposed Filter Widgets" below for how each widget type works.

The conference list is rendered as **cards** rather than a table. When the Trovato theme engine looks for a template to render a gather query, it first checks for a query-specific override at `templates/gather/query--{query_id}.html` before falling back to the generic `query.html`. The importer ships `templates/gather/query--ritrovo.upcoming_conferences.html`, which extends `query.html` and overrides the `query_content` block with a card grid:

```
.conf-card
  ├── title (linked to /item/{id})
  ├── dates (field_start_date – field_end_date)
  ├── location (field_city, field_country)
  ├── description (field_description, if present)
  └── actions
        ├── View details → /item/{id}
        ├── Website ↗ (field_url, if present)
        └── CFP open until {date} — Submit a talk (if field_cfp_url + field_cfp_end_date)
```

#### ritrovo.open_cfps

Conferences currently accepting talk proposals, sorted by deadline (soonest first).

```
Hard filters:   field_cfp_end_date ≥ current_date
                field_cfp_url is_not_null
Sort:           field_cfp_end_date ASC (nulls last)
```

The `is_not_null` filter on `field_cfp_url` ensures only conferences with a submission link are shown — no CFP URL means there's nothing to link to.

#### ritrovo.by_topic

Upcoming conferences filtered by a single topic UUID, including all descendant topics via recursive CTE.

```
Hard filters:   field_start_date ≥ current_date
                field_topics has_tag_or_descendants = url_arg("topic")
Sort:           field_start_date ASC
```

This gather is driven by a **gather route alias** declared in the query's `display.routes` config. The `/topics/{slug}` route resolves the slug to a tag UUID via `category_tag.slug` and redirects to `/gather/ritrovo.by_topic?topic=<uuid>`. The `url_arg("topic")` value reads the `?topic=` query-string parameter at request time.

#### ritrovo.by_country and ritrovo.by_city

Location gathers driven by URL path segments.

```
ritrovo.by_country:
  Hard filters:  field_start_date ≥ current_date
                 field_country equals url_arg("country")

ritrovo.by_city:
  Hard filters:  field_start_date ≥ current_date
                 field_country equals url_arg("country")
                 field_city    equals url_arg("city")
```

Two separate gathers are used (rather than one gather with an optional city filter) to avoid the empty-string problem: if a single gather had an optional `url_arg("city")` filter, a request without `?city=` would resolve to an empty string, which would match no conferences — not the same as "all conferences in this country". The two-gather design sidesteps this entirely: the gather route aliases map `/location/{country}` to `by_country` and `/location/{country}/{city}` to `by_city`.

### Exposed Filter Widgets

The `ritrovo.upcoming_conferences` gather has four exposed filters, each using a different widget type. Without explicit widget configuration, every exposed filter renders as a plain text input — acceptable for free-text searches but awkward for boolean fields and fields with a small set of known values.

Each filter declares its widget in the `"widget"` key of its filter JSON:

```json
{
    "field": "fields.field_online",
    "operator": "equals",
    "value": null,
    "exposed": true,
    "exposed_label": "Online Only",
    "widget": { "type": "boolean" }
}
```

The `"widget"` key is optional — omitting it is equivalent to `{ "type": "text" }`. Existing query definitions without a `"widget"` key continue to work unchanged.

#### Boolean widget — `field_online`

```json
"widget": { "type": "boolean" }
```

Renders a `<select>` with three options: **Any** (empty, filter skipped), **Yes** (`true`), **No** (`false`). When the user chooses "Yes", the form submits `?fields.field_online=true`. The gather's `parse_filter_params` parses `"true"` as a JSON boolean, producing a `FilterValue::Boolean(true)` that the `equals` operator compares correctly.

This is the right widget for any field that stores `true`/`false` — it's more usable than a text box asking users to type the word "true".

#### Taxonomy select widget — `field_topics`

```json
"widget": { "type": "taxonomy_select", "vocabulary": "topics" }
```

Renders a `<select>` populated from the `topic` category vocabulary. The kernel loads all tags with their hierarchy depth using `CategoryService::list_tags_with_depth()`, which returns them in DFS pre-order so that each parent immediately precedes its children. Child terms are indented with non-breaking spaces proportional to depth:

```
Any
Languages
  Systems
    Rust
    C++
    .NET
  JVM
    Java
  Scripting
    Python
    JavaScript
    ...
Infrastructure
  Cloud & DevOps
  ...
```

When the user selects "Systems", the form submits the UUID of the "Systems" tag. The `has_tag_or_descendants` operator then expands that UUID to include "Rust", "C++", ".NET", and any future child terms — the user selected a high-level concept and automatically gets all its concrete children.

The taxonomy select is the right widget whenever a filter uses `has_tag` or `has_tag_or_descendants`: it shows meaningful human labels, not raw UUIDs.

#### Dynamic options widget — `field_country` and `field_language`

```json
"widget": {
    "type": "dynamic_options",
    "source_field": "fields.field_country",
    "autocomplete_threshold": 30
}
```

The kernel queries distinct non-null, non-empty values for `fields.field_country` across all published `conference` items (up to 200), caches the list for five minutes, and passes it to the renderer. The renderer then chooses:

- **≤ threshold (30)** — a `<select>` dropdown listing every distinct value with an "Any" option at the top.
- **> threshold (30)** — a `<input type="text">` with a `<datalist>` for browser-native autocomplete. The user can type freely and the browser offers matching suggestions.

This dual-mode design handles two real situations: a small conference dataset early on (a handful of countries → clean dropdown) and a large dataset later (many countries → autocomplete).

Both `field_country` and `field_language` use the `equals` operator, not `contains`. This is intentional: the dropdown and datalist surface exact values that are already in the database, so filtering by exact match is correct — there's no reason to do a partial `ILIKE` search when you're selecting from a known list. A text-input filter with `contains` was the old behaviour before widgets were introduced.

#### Widget rendering paths

The widget data is pre-fetched once per request in `preload_widget_data()` — taxonomy tags from the database, distinct values from the cache — and then passed to whichever rendering path is active:

- **Tera theme templates** receive the widget type and option list as structured JSON in the `exposed_filters` context variable and branch on `filter.widget_type`.
- **Fallback HTML renderer** (`render_exposed_filter_form`) generates the same HTML directly in Rust when no theme template exists.

Both paths produce identical HTML, so switching between a custom template and the default fallback does not change the widget behaviour.

### The /conferences, /cfps, and /location Routes

All browse routes are served through a combination of gather route aliases, URL aliases, and `canonical_url` — no plugin-specific kernel code required.

**Location routes** are declared as gather route aliases in the query YAML:

```yaml
# gather_query.ritrovo.by_country.yml (display section)
display:
  routes:
  - path: /location/{country}
    params:
    - segment: country
      param: country
```

```yaml
# gather_query.ritrovo.by_city.yml (display section)
display:
  routes:
  - path: /location/{country}/{city}
    params:
    - segment: country
      param: country
    - segment: city
      param: city
```

These use **pass-through** parameter mapping — the path segment values are URL-encoded and forwarded directly as gather query parameters:

```
GET /location/Germany        →  307  /gather/ritrovo.by_country?country=Germany
GET /location/Germany/Berlin →  307  /gather/ritrovo.by_city?country=Germany&city=Berlin
```

**`/conferences` and `/cfps`** work differently. The `ritrovo.upcoming_conferences` and `ritrovo.open_cfps` gather queries set a `canonical_url` field in their display configuration:

```json
{ "canonical_url": "/conferences" }
{ "canonical_url": "/cfps" }
```

When the gather route handler serves a query that has `canonical_url` set, it uses that path as `base_path` for all generated URLs — form actions, pager links, and filter parameters all stay on the friendly path. Filtering by country and navigating to page 2 produces `GET /conferences?fields.field_country=Germany&page=2`, not `GET /gather/ritrovo.upcoming_conferences?…`.

The URL alias for `/conferences` → `/gather/ritrovo.upcoming_conferences` is defined in the tutorial config YAML and imported via `config import`. The config import uses `ON CONFLICT DO UPDATE`, so re-importing safely updates the alias if it already exists from Part 1.

> **Why not a dedicated route?** The earlier design used a thin Rust handler at `GET /conferences` that called `execute_and_render("ritrovo.upcoming_conferences", …, "/conferences")`. This was a kernel minimality violation — feature plumbing with no logic beyond setting a path string. `canonical_url` and `display.routes` express the same intent in configuration, with no Rust required.

### Sealing the Raw Gather URLs

Even with `canonical_url` set, `/gather/ritrovo.upcoming_conferences` is still a valid path — the gather engine serves it too. A user arriving there (via a bookmark, browser history, or an old link) would see the filter form submit back to the raw gather URL, breaking the stable-URL contract.

The kernel handles this automatically: when the gather service loads queries from the database, its `sync_canonical_redirects()` method creates 301 redirects for any query that has a `canonical_url` in its display config:

```
/gather/ritrovo.upcoming_conferences  →  301  /conferences
/gather/ritrovo.open_cfps             →  301  /cfps
```

The kernel's redirects middleware intercepts these paths before routing and sends the browser to the canonical URL. Any subsequent filter submission or page navigation then stays on `/conferences` or `/cfps` as intended.

No plugin code is needed for this — the redirects are created by the kernel whenever it detects a `canonical_url` on a gather query, whether that query was imported via config or created through the admin UI.

Gather route aliases are **kernel infrastructure**, not plugin-gated. They are registered at startup from whatever gather queries exist in the database — if the queries are present (via config import), the routes work regardless of whether the plugin that populated the data is enabled. This is consistent with the principle that browse routes are configuration, not plugin features.

> **Note:** Because gather route aliases are built at startup, changes to `display.routes` in the config YAML require a config reimport followed by a server restart to take effect.

### Configuration vs Plugin: The Division of Labor

The Ritrovo tutorial demonstrates a clean separation of concerns:

- **Configuration** (YAML files, imported via `config import`): taxonomy, gather queries, URL aliases. These are core Trovato concepts with first-class config support — they can be exported from one site and imported to another, version-controlled in git, and edited without touching plugin code.

- **Plugin** (WASM module, runs in sandbox): discovering taxonomy UUIDs from the imported config, fetching external data from the confs.tech API, validating and deduplicating conferences, queue management. These are import-specific operations that the kernel doesn't provide.

This separation means you can modify the gather query definitions (add new filters, change sort orders, update display config) by editing the YAML files and re-importing — no plugin rebuild required. The plugin only needs to change when the *import logic* changes (e.g. a new field from the upstream API, a different dedup strategy).

> **Design principle:** The core kernel enables. Plugins implement. If it's a feature definition (a gather query, a taxonomy, a URL alias), it belongs in configuration. If it's an integration with an external system, it belongs in a plugin.

---

With the taxonomy, gather queries, browse routes, and filter widgets in place, Ritrovo visitors can:

- Browse all upcoming conferences at `/conferences`, filtering with purpose-built controls:
  - A hierarchical topic selector (Any → Languages → Rust)
  - A country dropdown or autocomplete based on values actually in the database
  - An Online Only selector (Any / Yes / No)
  - A language dropdown or autocomplete
- Drill into a topic tree at `/topics/rust` (or `/topics/languages` for everything under that branch)
- Find open CFPs at `/cfps`
- Filter by country at `/location/Germany` and by city at `/location/Germany/Berlin`

[<img src="images/2.1-conferences-listing.png" width="600" alt="Figure 2.1: Upcoming Conferences with exposed filter bar">](images/2.1-conferences-listing.png)

[<img src="images/2.2-conferences-filtered.png" width="600" alt="Figure 2.2: Conferences filtered by Security topic, non-online, English language">](images/2.2-conferences-filtered.png)

The gather engine handles filtering, pagination, widget data loading, and rendering. The query definitions, taxonomy, and URL aliases are all managed as configuration — the plugin's only job is discovering taxonomy UUIDs and importing conference data from the external API.
