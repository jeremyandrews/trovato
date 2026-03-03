# Story 33.2: Cron-Driven Conference Import

Status: not started

## Story

As a **Ritrovo site operator**,
I want the `ritrovo_importer` plugin to automatically fetch and import conferences from confs.tech daily,
so that the site stays current without manual data entry.

## Acceptance Criteria

1. `tap_cron` fetches conference JSON from confs.tech for all 26 topic files via `http_request()` host function
2. Conditional HTTP requests use `If-None-Match` / ETag headers; a 304 response skips processing for that topic
3. Each fetched conference is pushed onto the import queue via the Queue API (`tap_queue_info` / `tap_queue_worker`)
4. Queue worker validates each entry: required fields present (`name`, `startDate`, `endDate`), date format valid (YYYY-MM-DD), end_date >= start_date
5. Invalid entries are logged with reason (field name + rule violated) and skipped — never silently dropped
6. Valid conferences are created as published `conference` Items with correct field mappings (see table in brief)
7. `source_id` (slugified `name + start_date + city`) is stored in `field_source_id`; existing items with matching `source_id` are updated when source data changes (diff on mapped fields)
8. `tap_plugin_install` triggers a full historical import (all available years, all topics) — not just current year
9. Tutorial section covers: Queue API pattern, field mapping, dedup strategy, conditional HTTP
10. `trovato-test` blocks assert: queue push on cron fire, validation rejects bad data, dedup skips unchanged items, new item created with correct fields

## Tasks / Subtasks

- [ ] Implement `tap_cron` fetch loop (AC: #1, #2)
  - [ ] Iterate 26 topic slugs; build URL `https://raw.githubusercontent.com/tech-conferences/conference-data/main/conferences/{year}/{topic}.json`
  - [ ] Call `http_request()` with stored ETag in `If-None-Match` header
  - [ ] On 304: skip. On 200: store new ETag via `config_set()`, push raw JSON to queue
- [ ] Implement `tap_queue_info` to declare queue name and concurrency (AC: #3)
- [ ] Implement `tap_queue_worker` validation + item creation (AC: #4, #5, #6, #7)
  - [ ] Deserialize queue payload (topic slug + raw conference JSON array)
  - [ ] For each entry: validate required fields, validate date format and ordering
  - [ ] Compute `source_id`: slugify `{name}-{start_date}-{city}`
  - [ ] Look up existing item by `fields->>'field_source_id'` via `db_query()`
  - [ ] If found and fields unchanged: skip. If found and changed: update via `item_update()`. If not found: create via `item_create()`
  - [ ] On validation failure: `log_warn!` with entry name + reason, continue loop
- [ ] Implement full historical import in `tap_plugin_install` (AC: #8)
  - [ ] Fetch years 2015–current from confs.tech (skip 404s gracefully)
  - [ ] Queue all fetched data same as cron path (reuse queue worker)
- [ ] Write tutorial section 2.2 (AC: #9)
- [ ] Write `trovato-test` blocks (AC: #10)

## Dev Notes

### confs.tech Data Source

GitHub raw URL pattern:
```
https://raw.githubusercontent.com/tech-conferences/conference-data/main/conferences/{year}/{topic}.json
```

26 topics: `accessibility`, `android`, `api`, `css`, `data`, `devops`, `dotnet`, `general`, `graphql`, `ios`, `iot`, `java`, `javascript`, `kotlin`, `leadership`, `networking`, `opensource`, `performance`, `php`, `product`, `python`, `rust`, `security`, `testing`, `typescript`, `ux`.

Years available: 2015 through current year. Some topic+year combos return 404 (topic didn't exist yet). Treat 404 as empty, not as error.

### Field Mapping

| confs.tech field | `conference` item field | Notes |
|---|---|---|
| `name` | title (item column) | Direct |
| `url` | `field_url` | Direct |
| `startDate` | `field_start_date` | YYYY-MM-DD, already correct format |
| `endDate` | `field_end_date` | YYYY-MM-DD |
| `city` | `field_city` | May contain "City, State" |
| `country` | `field_country` | Various formats |
| `online` | `field_online` | boolean → "1" or absent |
| `cfpUrl` | `field_cfp_url` | Nullable |
| `cfpEndDate` | `field_cfp_end_date` | Nullable, YYYY-MM-DD |
| `locales[0]` | `field_language` | ISO-ish (EN, FR, etc.) |
| (topic slug) | `field_source_id` suffix | Used in source_id; `field_topics` added in Story 33.3 |
| — | `field_source_id` | `slugify("{name}-{start_date}-{city}")` |

### source_id Dedup Strategy

`source_id = slugify(name + "-" + start_date + "-" + (city or "online"))`.

On import:
```
SELECT id FROM item WHERE type = 'conference' AND fields->>'field_source_id' = $1
```

If the row exists, compare the mapped field values. If any differ, call `item_update`. If identical, skip. This means a cron run on a day with no upstream changes is a pure read — no writes.

### Queue API Pattern

`tap_queue_info` declares:
- queue name: `"ritrovo_import"`
- max concurrency: 4 (parallel topic files)

`tap_cron` pushes one payload per topic per year:
```json
{"topic": "rust", "year": 2026, "conferences": [...]}
```

`tap_queue_worker` processes one payload per call.

### ETag Storage

Use `config_set("ritrovo_importer.etag.{topic}.{year}", etag_value)` to persist ETags across cron runs. Use `config_get` to retrieve on next run.

### Host Functions Required

- `http_request(url, method, headers, body)` — fetch JSON
- `db_query(sql, params)` — look up existing items by source_id
- `item_create(type, title, fields, status)` — create new conference
- `item_update(id, fields)` — update changed fields
- `queue_push(queue_name, payload)` — enqueue from cron tap
- `config_get(key)` / `config_set(key, value)` — ETag persistence
- `log_info!` / `log_warn!` — structured logging

Verify all of these exist in `crates/plugin-sdk/src/host_functions.rs` before implementing. Add any missing ones.

### Date Validation Rules

- Format: matches `^\d{4}-\d{2}-\d{2}$`
- Range: year between 2010 and 2035 (anything outside is data error)
- Ordering: `end_date >= start_date`
- `cfp_end_date`, if present: must be <= `start_date` (CFP closes before conference starts)

### Key Files

- `plugins/ritrovo_importer/src/lib.rs` — all tap implementations
- `crates/plugin-sdk/src/host_functions.rs` — verify/add host functions
- `crates/kernel/src/host/` — kernel-side host function implementations
- `docs/tutorial/part-02-ritrovo-importer.md` — section 2.2

### Dependencies

- Story 33.1 complete (plugin scaffold exists)
- `item_create` / `item_update` host functions must exist in plugin SDK
- Queue tap infrastructure must be functional in kernel

### References

- confs.tech repo: `https://github.com/tech-conferences/conference-data`
- Existing HTTP host function: `crates/kernel/src/host/http.rs` (if exists)
- Queue tap: `crates/kernel/src/tap.rs` (tap_queue_info, tap_queue_worker)

## Dev Agent Record

### Agent Model Used

### Debug Log References

### Completion Notes List

### File List
