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

```
trovato plugin new my_plugin
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

> **Note:** The `ritrovo_importer` plugin already ships with Trovato as a complete example. You don't need to scaffold it — instead, read through its source to understand the patterns, then use `trovato plugin install ritrovo_importer` to enable it.

### Installing and enabling a plugin

After building the `.wasm`:

```bash
cargo build --target wasm32-wasip1 -p ritrovo_importer --release
trovato plugin install ritrovo_importer
```

`plugin install` runs any pending SQL migrations, then marks the plugin as enabled. The next time the server starts (or via the admin UI at `/admin/plugins`), the plugin loads.

When the plugin is enabled **for the first time**, the kernel calls `tap_install`. You'll see this in the server logs:

```
INFO ritrovo_importer: ritrovo_importer installed — import will begin on next cron cycle
```

> **Note:** `tap_install` fires only once — on first enable. If `ritrovo_importer` was already installed automatically at server startup (the default), you won't see this message in existing environments. To see it, disable the plugin, uninstall it via the admin UI, re-enable it, and watch the logs.

### The four stubs: tap_install, tap_cron, tap_queue_info, tap_queue_worker

The generated scaffold includes stubs for the four taps the importer uses:

| Tap | When called | What it does |
|---|---|---|
| `tap_install` | Once, on first enable | Seeds initial data, logs confirmation |
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

Newly inserted conferences are created as **unpublished** (`status = 0`) on the live stage. This lets Ritrovo editors review and publish them before they appear in public gathers.

### Historical Import: tap_install

When the plugin is first enabled, `tap_install` runs a full historical backfill. It fetches every topic file for every year from 2015 to the current year, stores ETags, and pushes each successful response onto the queue:

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

With the importer running, Ritrovo will accumulate hundreds of conferences automatically. The next part covers building the taxonomy that lets visitors filter by topic and region.
