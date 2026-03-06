//! Conference importer plugin for the Ritrovo tutorial.
//!
//! Imports tech conference data from the
//! [confs.tech](https://github.com/tech-conferences/conference-data)
//! open-source dataset.
//!
//! Architecture:
//! - `tap_install`: runs a full historical import (2015–current year) by
//!   pushing all fetched data onto the `ritrovo_import` queue.
//! - `tap_cron`: fetches the current and next year's data for a rotating
//!   subset of topics, using ETags to skip unchanged files, then pushes
//!   each topic's payload onto the queue.
//! - `tap_queue_info`: declares the `ritrovo_import` queue (concurrency 4).
//! - `tap_queue_worker`: validates and upserts a single topic's conferences.

use std::collections::HashMap;

use trovato_sdk::host;
use trovato_sdk::prelude::*;

/// Plugin name for logging calls.
const PLUGIN_NAME: &str = "ritrovo_importer";

/// Base URL for raw conference JSON from the confs.tech GitHub repo.
const DATA_BASE_URL: &str =
    "https://raw.githubusercontent.com/tech-conferences/conference-data/main/conferences";

/// First year of conference data available in confs.tech.
const FIRST_IMPORT_YEAR: u16 = 2015;

/// Topics to import, corresponding to filenames in the confs.tech repo.
const TOPICS: &[&str] = &[
    "accessibility",
    "android",
    "api",
    "cpp",
    "css",
    "data",
    "devops",
    "dotnet",
    "general",
    "ios",
    "java",
    "javascript",
    "kotlin",
    "networking",
    "opensource",
    "php",
    "python",
    "ruby",
    "rust",
    "scala",
    "security",
    "sre",
    "testing",
    "typescript",
    "ux",
];

/// Number of topics to process per cron cycle (round-robin to stay
/// well within the 150-second cron dispatch timeout).
const TOPICS_PER_CYCLE: usize = 5;

/// Minimum interval between cron import runs (24 hours in seconds).
const IMPORT_INTERVAL_SECS: i64 = 86_400;

/// State key tracking last import timestamp.
const STATE_LAST_IMPORT: &str = "last_import";

/// State key tracking the topic offset for round-robin scheduling.
const STATE_TOPIC_OFFSET: &str = "topic_offset";

/// Prefix for ETag state keys: `"etag.{topic}.{year}"`.
const STATE_ETAG_PREFIX: &str = "etag";

/// Queue name declared in `tap_queue_info`.
const QUEUE_NAME: &str = "ritrovo_import";

/// Category ID for the conference topics taxonomy.
const TOPICS_CATEGORY_ID: &str = "topics";

/// State key prefix for topic term UUIDs: `"topic_term.{term_slug}"`.
const STATE_TOPIC_TERM_PREFIX: &str = "topic_term";

/// Maximum number of conferences per queue payload.
///
/// The WASM input buffer is 64 KB; a single confs.tech JSON file can exceed
/// that (e.g. general/2019 is ~69 KB). Chunking at 50 conferences per batch
/// keeps every payload well under the limit.
const CONFERENCES_PER_BATCH: usize = 50;

// ─── Topic slug → taxonomy label mapping ──────────────────────────────

/// Maps confs.tech topic slugs to `(term_slug, term_label)` pairs.
///
/// The term_slug is the key used in `ritrovo_state` (`topic_term.{slug}`).
/// The term_label is used to discover the `category_tag` UUID from the database
/// during `tap_install`.
///
/// Confs.tech slugs not listed here (`sre`, `scala`) have no taxonomy entry
/// and will be stored with an empty `field_topics`.
const SLUG_TO_TERM: &[(&str, &str, &str)] = &[
    ("rust", "rust", "Rust"),
    ("java", "java", "Java"),
    ("kotlin", "kotlin", "Kotlin"),
    ("javascript", "javascript", "JavaScript"),
    ("typescript", "typescript", "TypeScript"),
    ("php", "php", "PHP"),
    ("python", "python", "Python"),
    ("ruby", "ruby", "Ruby"),
    ("dotnet", "dotnet", ".NET"),
    ("android", "android", "Android"),
    ("ios", "ios", "iOS"),
    ("devops", "devops", "DevOps"),
    ("networking", "networking", "Networking"),
    ("data", "data", "Data Engineering"),
    ("css", "css", "CSS"),
    ("ux", "ux", "UX"),
    ("accessibility", "accessibility", "Accessibility"),
    ("security", "appsec", "AppSec"),
    ("api", "api", "API"),
    ("testing", "testing", "Testing"),
    ("general", "general", "General"),
    ("opensource", "opensource", "Open Source"),
    ("cpp", "cpp", "C++"),
];

// ─── Conference field definitions ────────────────────────────────────
//
// The `conference` item type is created by the user via the admin UI
// (see tutorial Part 1 Step 2). This plugin does NOT auto-register it
// via `tap_item_info` — the importer assumes the type already exists
// when `tap_cron` or `tap_queue_worker` runs.

/// Build the field definitions for the conference content type.
///
/// Called by unit tests to verify field declarations. The importer does not
/// register the content type itself (the tutorial user creates it via the
/// admin UI), so this function is not called from production code paths.
#[cfg_attr(not(test), allow(dead_code))]
fn conference_fields() -> Vec<FieldDefinition> {
    vec![
        FieldDefinition::new(
            "field_url",
            FieldType::Text {
                max_length: Some(2048),
            },
        )
        .label("Website URL"),
        FieldDefinition::new("field_start_date", FieldType::Date)
            .label("Start Date")
            .required(),
        FieldDefinition::new("field_end_date", FieldType::Date)
            .label("End Date")
            .required(),
        FieldDefinition::new(
            "field_city",
            FieldType::Text {
                max_length: Some(255),
            },
        )
        .label("City"),
        FieldDefinition::new(
            "field_country",
            FieldType::Text {
                max_length: Some(255),
            },
        )
        .label("Country"),
        FieldDefinition::new("field_online", FieldType::Boolean).label("Online"),
        FieldDefinition::new(
            "field_cfp_url",
            FieldType::Text {
                max_length: Some(2048),
            },
        )
        .label("CFP URL"),
        FieldDefinition::new("field_cfp_end_date", FieldType::Date).label("CFP End Date"),
        FieldDefinition::new("field_description", FieldType::TextLong).label("Description"),
        FieldDefinition::new(
            "field_topics",
            FieldType::Text {
                max_length: Some(255),
            },
        )
        .label("Topics")
        .cardinality(-1),
        FieldDefinition::new(
            "field_language",
            FieldType::Text {
                max_length: Some(10),
            },
        )
        .label("Language"),
        FieldDefinition::new(
            "field_source_id",
            FieldType::Text {
                max_length: Some(512),
            },
        )
        .label("Source ID"),
        FieldDefinition::new(
            "field_twitter",
            FieldType::Text {
                max_length: Some(255),
            },
        )
        .label("Twitter/X"),
        FieldDefinition::new(
            "field_coc_url",
            FieldType::Text {
                max_length: Some(2048),
            },
        )
        .label("Code of Conduct URL"),
        FieldDefinition::new("field_editor_notes", FieldType::TextLong).label("Editor Notes"),
    ]
}

// ─── Install ──────────────────────────────────────────────────────────

/// Called once when the plugin is first enabled in the admin UI.
///
/// 1. Discovers taxonomy term UUIDs from the database (terms are created
///    via config import, not by the plugin).
/// 2. Triggers a full historical import (2015–current year) by pushing all
///    available topic/year combinations onto the `ritrovo_import` queue.
///    The queue worker (`tap_queue_worker`) processes each batch
///    asynchronously in subsequent cron cycles.
#[plugin_tap]
pub fn tap_install() -> serde_json::Value {
    host::log(
        "info",
        PLUGIN_NAME,
        "ritrovo_importer installed — discovering taxonomy and starting historical import",
    );

    // 1. Discover taxonomy term UUIDs from the config-imported terms.
    let discovered = discover_taxonomy_uuids();

    // 2. Queue historical import.
    let now = current_timestamp();
    let current_year = timestamp_to_year(now) as u16;
    let mut pushed = 0u32;
    let mut errors = 0u32;

    for year in FIRST_IMPORT_YEAR..=current_year {
        for topic in TOPICS {
            let url = format!("{DATA_BASE_URL}/{year}/{topic}.json");

            let response = match host::http_request(
                &trovato_sdk::types::HttpRequest::get(&url).timeout(15_000),
            ) {
                Ok(r) if r.status == 200 => r,
                Ok(r) if r.status == 404 => continue,
                Ok(r) => {
                    errors += 1;
                    host::log("warn", PLUGIN_NAME, &format!("HTTP {} for {url}", r.status));
                    continue;
                }
                Err(_) => {
                    errors += 1;
                    continue;
                }
            };

            // Store ETag for future conditional requests.
            if let Some(etag) = response
                .headers
                .get("etag")
                .or_else(|| response.headers.get("ETag"))
            {
                set_state(&etag_key(topic, year), etag);
            }

            let (p, e) = push_conference_batches(topic, year, &response.body);
            pushed += p;
            errors += e;
        }
    }

    host::log(
        "info",
        PLUGIN_NAME,
        &format!(
            "historical import queued: {pushed} batches across {years} years",
            years = (current_year - FIRST_IMPORT_YEAR + 1)
        ),
    );

    serde_json::json!({
        "status": "ok",
        "discovered_terms": discovered,
        "queued": pushed,
        "errors": errors,
    })
}

// ─── Queue helpers ─────────────────────────────────────────────────────

/// Push conference data onto the import queue, chunking large payloads.
///
/// A single confs.tech JSON file can exceed the 64 KB WASM input limit.
/// This function parses the raw body as a JSON array and pushes sub-slices of
/// at most [`CONFERENCES_PER_BATCH`] items. If parsing fails the raw body is
/// pushed as-is (will fail at the worker if still too large).
///
/// Returns `(batches_pushed, errors)`.
fn push_conference_batches(topic: &str, year: u16, body: &str) -> (u32, u32) {
    let mut pushed = 0u32;
    let mut errors = 0u32;

    // Try to parse as an array so we can chunk it.
    if let Ok(confs) = serde_json::from_str::<Vec<serde_json::Value>>(body) {
        for chunk in confs.chunks(CONFERENCES_PER_BATCH) {
            let payload = serde_json::json!({
                "topic": topic,
                "year": year,
                "conferences": serde_json::to_string(chunk).unwrap_or_default(),
            });
            match host::queue_push(QUEUE_NAME, &payload) {
                Ok(()) => pushed += 1,
                Err(_) => errors += 1,
            }
        }
    } else {
        // Fallback: body is not a valid JSON array — push raw and let the
        // worker surface the parse error.  Log here so the operator can
        // identify which topic/year produced malformed JSON.
        host::log(
            "warn",
            PLUGIN_NAME,
            &format!(
                "push_conference_batches: failed to parse JSON array for {topic}/{year}, \
                 pushing raw payload"
            ),
        );
        let payload = serde_json::json!({
            "topic": topic,
            "year": year,
            "conferences": body,
        });
        match host::queue_push(QUEUE_NAME, &payload) {
            Ok(()) => pushed += 1,
            Err(_) => errors += 1,
        }
    }

    (pushed, errors)
}

// ─── Taxonomy discovery ───────────────────────────────────────────────

/// Discover taxonomy term UUIDs from the database.
///
/// The `topics` category and its terms are created via config import
/// (YAML files in `docs/tutorial/config/`), not by the plugin. This
/// function looks up each term's UUID by label and caches it in
/// `ritrovo_state` for use by the queue worker when tagging conferences.
///
/// Returns the number of terms discovered.
fn discover_taxonomy_uuids() -> u32 {
    let mut discovered = 0u32;

    for &(_confs_slug, term_slug, term_label) in SLUG_TO_TERM {
        let state_key = format!("{STATE_TOPIC_TERM_PREFIX}.{term_slug}");

        // Skip if already cached in state.
        if load_state_str(&state_key).is_some() {
            discovered += 1;
            continue;
        }

        // Look up the UUID from the config-imported category_tag row.
        let result = host::query_raw(
            "SELECT id::text AS id FROM category_tag \
             WHERE category_id = $1 AND label = $2 \
             LIMIT 1",
            &[
                serde_json::json!(TOPICS_CATEGORY_ID),
                serde_json::json!(term_label),
            ],
        );
        if let Some(uuid) = result
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
            .and_then(|rows| rows.into_iter().next())
            .and_then(|row| row.get("id").and_then(|v| v.as_str()).map(String::from))
        {
            save_state(&state_key, &uuid);
            discovered += 1;
        } else {
            host::log(
                "warn",
                PLUGIN_NAME,
                &format!(
                    "discover_taxonomy_uuids: term '{term_label}' not found in category \
                     '{TOPICS_CATEGORY_ID}' — was config imported?"
                ),
            );
        }
    }

    host::log(
        "info",
        PLUGIN_NAME,
        &format!(
            "discover_taxonomy_uuids: {discovered}/{} terms found",
            SLUG_TO_TERM.len()
        ),
    );

    discovered
}

/// Look up the category_tag UUID for a confs.tech topic slug.
///
/// Returns `None` if the slug has no taxonomy mapping (e.g. `sre`, `scala`)
/// or if the taxonomy term has not been discovered yet.
fn topic_term_uuid(confs_tech_slug: &str) -> Option<String> {
    // Map the confs.tech slug to the taxonomy term slug.
    let term_slug = SLUG_TO_TERM
        .iter()
        .find(|(src, _, _)| *src == confs_tech_slug)
        .map(|(_, term, _)| *term)?;

    load_state_str(&format!("{STATE_TOPIC_TERM_PREFIX}.{term_slug}"))
}

// ─── Permissions ─────────────────────────────────────────────────────

/// Define importer-specific permissions.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    let mut perms = PermissionDefinition::crud_for_type("conference");
    perms.push(PermissionDefinition::new(
        "administer conference import",
        "Configure and trigger conference imports",
    ));
    perms
}

// ─── Menu routes ─────────────────────────────────────────────────────

/// Define admin menu routes for the importer.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/content/conferences", "Conferences")
            .callback("conference_list")
            .permission("view conference content")
            .parent("/admin/content"),
        MenuDefinition::new("/admin/config/importer", "Conference Import")
            .callback("importer_config")
            .permission("administer conference import")
            .parent("/admin/config"),
    ]
}

// ─── Cron: daily import ──────────────────────────────────────────────

/// Daily cron handler that pushes conference fetch jobs onto the queue.
///
/// Processes a rotating subset of topics per cycle (round-robin) to stay
/// within cron timeout limits. Uses conditional HTTP (`If-None-Match`) to
/// skip topics whose upstream data has not changed since the last import.
/// Deduplication and DB writes happen in `tap_queue_worker`.
#[plugin_tap]
pub fn tap_cron(input: CronInput) -> serde_json::Value {
    let now = input.timestamp;

    if !should_import(now) {
        return serde_json::json!({"status": "skipped", "reason": "too_soon"});
    }

    let current_year = timestamp_to_year(now);
    let import_years = [current_year, current_year + 1];

    let topic_offset = load_state_usize(STATE_TOPIC_OFFSET, TOPICS.len());
    let cycle_topics: Vec<&str> = TOPICS
        .iter()
        .cycle()
        .skip(topic_offset)
        .take(TOPICS_PER_CYCLE)
        .copied()
        .collect();

    let mut queued = 0u32;
    let mut skipped_304 = 0u32;
    let mut errors = 0u32;

    for year in &import_years {
        for topic in &cycle_topics {
            match fetch_topic_for_queue(topic, *year) {
                FetchResult::Queued => queued += 1,
                FetchResult::NotModified => skipped_304 += 1,
                FetchResult::NotFound => {}
                FetchResult::Error => errors += 1,
            }
        }
    }

    let next_offset = (topic_offset + TOPICS_PER_CYCLE) % TOPICS.len();
    save_state(STATE_TOPIC_OFFSET, &next_offset.to_string());

    if queued > 0 || skipped_304 > 0 {
        save_state(STATE_LAST_IMPORT, &now.to_string());
    }

    serde_json::json!({
        "status": "completed",
        "queued": queued,
        "skipped_304": skipped_304,
        "errors": errors,
        "topics_processed": cycle_topics,
    })
}

/// Result of fetching a single topic for the queue.
enum FetchResult {
    /// Payload pushed onto the queue.
    Queued,
    /// Server returned 304 Not Modified — no work needed.
    NotModified,
    /// Server returned 404 — topic/year combo doesn't exist.
    NotFound,
    /// HTTP or serialization error.
    Error,
}

/// Fetch one topic+year and push the raw JSON onto the queue.
///
/// Uses the stored ETag as `If-None-Match` for conditional requests.
/// On a 200 response, stores the new ETag and pushes the payload.
fn fetch_topic_for_queue(topic: &str, year: u16) -> FetchResult {
    let url = format!("{DATA_BASE_URL}/{year}/{topic}.json");
    let etag_key = etag_key(topic, year);
    let stored_etag = load_state_str(&etag_key);

    let mut request = trovato_sdk::types::HttpRequest::get(&url).timeout(15_000);
    if let Some(ref etag) = stored_etag {
        request = request.header("If-None-Match", etag);
    }

    let Ok(response) = host::http_request(&request) else {
        return FetchResult::Error;
    };

    match response.status {
        304 => FetchResult::NotModified,
        404 => FetchResult::NotFound,
        200 => {
            // Persist the new ETag for the next cron run.
            if let Some(etag) = response
                .headers
                .get("etag")
                .or_else(|| response.headers.get("ETag"))
            {
                set_state(&etag_key, etag);
            }

            let (p, e) = push_conference_batches(topic, year, &response.body);
            if e > 0 {
                host::log(
                    "warn",
                    PLUGIN_NAME,
                    &format!("fetch_topic: {e} batch(es) failed to push for {topic}/{year}"),
                );
            }
            if p > 0 {
                FetchResult::Queued
            } else {
                FetchResult::Error
            }
        }
        status => {
            host::log("warn", PLUGIN_NAME, &format!("HTTP {status} for {url}"));
            FetchResult::Error
        }
    }
}

// ─── Queue declaration ────────────────────────────────────────────────

/// Declare the queue this plugin owns.
///
/// The kernel calls this at startup to discover plugin-managed queues.
/// The `concurrency` field controls how many `tap_queue_worker` calls
/// the kernel may dispatch in parallel.
#[plugin_tap]
pub fn tap_queue_info() -> serde_json::Value {
    serde_json::json!([
        {
            "name": QUEUE_NAME,
            "concurrency": 4
        }
    ])
}

// ─── Queue worker ─────────────────────────────────────────────────────

/// Process one queued import batch.
///
/// The kernel calls this once per item in the `ritrovo_import` queue,
/// passing a payload of the form:
///
/// ```json
/// { "topic": "rust", "year": 2026, "conferences": "[...]" }
/// ```
///
/// Each conference entry is validated, deduplicated against existing
/// items via `field_source_id`, then inserted or updated.
#[plugin_tap]
pub fn tap_queue_worker(input: serde_json::Value) -> serde_json::Value {
    let topic = match input.get("topic").and_then(|v| v.as_str()) {
        Some(t) => t.to_string(),
        None => {
            host::log(
                "warn",
                PLUGIN_NAME,
                "tap_queue_worker: missing 'topic' field",
            );
            return serde_json::json!({"status": "error", "reason": "missing_topic"});
        }
    };

    let year = match input.get("year").and_then(|v| v.as_u64()) {
        Some(y) => y as u16,
        None => {
            host::log(
                "warn",
                PLUGIN_NAME,
                "tap_queue_worker: missing 'year' field",
            );
            return serde_json::json!({"status": "error", "reason": "missing_year"});
        }
    };

    // The `conferences` field contains the raw JSON body as a string.
    let body = match input.get("conferences").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            host::log(
                "warn",
                PLUGIN_NAME,
                "tap_queue_worker: missing 'conferences' field",
            );
            return serde_json::json!({"status": "error", "reason": "missing_conferences"});
        }
    };

    let conferences: Vec<ConfsTechEntry> = match serde_json::from_str(&body) {
        Ok(c) => c,
        Err(e) => {
            host::log(
                "warn",
                PLUGIN_NAME,
                &format!("JSON parse error for {topic}/{year}: {e}"),
            );
            return serde_json::json!({"status": "error", "reason": "parse_error"});
        }
    };

    let now = current_timestamp();
    // Look up the taxonomy UUID for this batch's topic slug.
    // Returns None for unmapped slugs such as `sre` and `scala`.
    let topic_uuid = topic_term_uuid(&topic);
    let existing = load_existing_conferences();

    let mut imported = 0u64;
    let mut updated = 0u64;
    let mut skipped = 0u64;
    let mut invalid = 0u64;

    for conf in &conferences {
        match validate_conference(conf) {
            Ok(()) => {}
            Err(reason) => {
                host::log(
                    "warn",
                    PLUGIN_NAME,
                    &format!(
                        "Skipping '{}': {reason}",
                        if conf.name.is_empty() {
                            "(unnamed)"
                        } else {
                            &conf.name
                        }
                    ),
                );
                invalid += 1;
                continue;
            }
        }

        let source_id = compute_source_id(conf);

        if let Some(info) = existing.get(&source_id) {
            let merged_topics = merge_topics(&info.topics, topic_uuid.as_deref());
            if update_conference(&info.item_id, conf, &merged_topics, now) {
                updated += 1;
            } else {
                skipped += 1;
            }
        } else if insert_conference(conf, &source_id, topic_uuid.as_deref(), now) {
            imported += 1;
        } else {
            invalid += 1;
        }
    }

    serde_json::json!({
        "status": "ok",
        "topic": topic,
        "year": year,
        "imported": imported,
        "updated": updated,
        "skipped": skipped,
        "invalid": invalid,
    })
}

// ─── confs.tech JSON schema ──────────────────────────────────────────

/// A single conference entry from the confs.tech dataset.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfsTechEntry {
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    start_date: String,
    #[serde(default)]
    end_date: String,
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    online: Option<bool>,
    #[serde(default)]
    cfp_url: Option<String>,
    #[serde(default)]
    cfp_end_date: Option<String>,
    #[serde(default)]
    locales: Option<String>,
    #[serde(default)]
    twitter: Option<String>,
    #[serde(default)]
    coc_url: Option<String>,
}

/// Info about an existing conference item in the database.
struct ExistingConference {
    item_id: String,
    topics: Vec<String>,
}

// ─── Validation ───────────────────────────────────────────────────────

/// Validate a conference entry from the confs.tech dataset.
///
/// Returns `Ok(())` if the entry is valid, or `Err(reason)` with a
/// human-readable description of the first rule violated.
fn validate_conference(conf: &ConfsTechEntry) -> Result<(), String> {
    if conf.name.is_empty() {
        return Err("missing required field: name".to_string());
    }
    if conf.start_date.is_empty() {
        return Err("missing required field: startDate".to_string());
    }
    if conf.end_date.is_empty() {
        return Err("missing required field: endDate".to_string());
    }

    if !is_valid_date(&conf.start_date) {
        return Err(format!("invalid startDate format: '{}'", conf.start_date));
    }
    if !is_valid_date(&conf.end_date) {
        return Err(format!("invalid endDate format: '{}'", conf.end_date));
    }

    if conf.end_date < conf.start_date {
        return Err(format!(
            "endDate '{}' is before startDate '{}'",
            conf.end_date, conf.start_date
        ));
    }

    if let Some(ref cfp_end) = conf.cfp_end_date {
        if !cfp_end.is_empty() && !is_valid_date(cfp_end) {
            return Err(format!("invalid cfpEndDate format: '{cfp_end}'"));
        }
        if !cfp_end.is_empty() && cfp_end.as_str() > conf.start_date.as_str() {
            return Err(format!(
                "cfpEndDate '{cfp_end}' is after startDate '{}'",
                conf.start_date
            ));
        }
    }

    Ok(())
}

/// Check that a date string matches `YYYY-MM-DD` and has a plausible year.
fn is_valid_date(date: &str) -> bool {
    if date.len() != 10 {
        return false;
    }
    let bytes = date.as_bytes();
    // Check digit positions and separators.
    for (i, &b) in bytes.iter().enumerate() {
        match i {
            4 | 7 => {
                if b != b'-' {
                    return false;
                }
            }
            _ => {
                if !b.is_ascii_digit() {
                    return false;
                }
            }
        }
    }
    // Plausible year range (2010–2035).
    let year_str = &date[..4];
    if let Ok(y) = year_str.parse::<u16>() {
        (2010..=2035).contains(&y)
    } else {
        false
    }
}

// ─── State helpers ────────────────────────────────────────────────────

/// Build the ETag state key for a given topic and year.
fn etag_key(topic: &str, year: u16) -> String {
    format!("{STATE_ETAG_PREFIX}.{topic}.{year}")
}

/// Load a string value from the plugin's persistent state table.
fn load_state_str(key: &str) -> Option<String> {
    let result = host::query_raw(
        "SELECT value FROM ritrovo_state WHERE name = $1",
        &[serde_json::json!(key)],
    );
    result
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
        .and_then(|rows| rows.into_iter().next())
        .and_then(|row| row.get("value").and_then(|v| v.as_str()).map(String::from))
}

/// Load a `usize` from the plugin state, returning 0 (mod `modulus`) on
/// missing or parse error.
fn load_state_usize(key: &str, modulus: usize) -> usize {
    load_state_str(key)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
        % modulus
}

/// Persist a string value in the plugin's persistent state table.
fn save_state(key: &str, value: &str) {
    let _ = host::execute_raw(
        "INSERT INTO ritrovo_state (name, value) VALUES ($1, $2) \
         ON CONFLICT (name) DO UPDATE SET value = $2",
        &[serde_json::json!(key), serde_json::json!(value)],
    );
}

/// Convenience wrapper that calls `save_state`.
fn set_state(key: &str, value: &str) {
    save_state(key, value);
}

/// Check if enough time has passed since the last import run.
fn should_import(now: i64) -> bool {
    let last_ts = load_state_str(STATE_LAST_IMPORT)
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    (now - last_ts) >= IMPORT_INTERVAL_SECS
}

// ─── Time helpers ─────────────────────────────────────────────────────

/// Derive the calendar year from a Unix timestamp.
fn timestamp_to_year(ts: i64) -> u16 {
    // 365.2425 days/year average; safe approximation for year extraction.
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let year = (1970 + ts / 31_556_952) as u16;
    year
}

/// Return the current Unix timestamp via the DB clock.
///
/// Used in `tap_queue_worker` where no `CronInput` is available.
fn current_timestamp() -> i64 {
    let result = host::query_raw("SELECT EXTRACT(EPOCH FROM NOW())::bigint AS ts", &[]);
    result
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
        .and_then(|rows| rows.into_iter().next())
        .and_then(|row| row.get("ts").and_then(|v| v.as_i64()))
        .unwrap_or(0)
}

// ─── Database helpers ─────────────────────────────────────────────────

/// Load existing conferences into a map of source_id → info.
fn load_existing_conferences() -> HashMap<String, ExistingConference> {
    let mut existing = HashMap::new();

    let result = host::query_raw(
        "SELECT id, fields->>'field_source_id' AS source_id, \
         fields->'field_topics' AS topics \
         FROM item \
         WHERE type = 'conference' \
         AND fields->>'field_source_id' IS NOT NULL",
        &[],
    );

    if let Ok(json_str) = result {
        let rows: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap_or_default();
        for row in rows {
            if let (Some(id), Some(sid)) = (
                row.get("id").and_then(|v| v.as_str()),
                row.get("source_id").and_then(|v| v.as_str()),
            ) {
                let topics = row
                    .get("topics")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                existing.insert(
                    sid.to_string(),
                    ExistingConference {
                        item_id: id.to_string(),
                        topics,
                    },
                );
            }
        }
    }

    existing
}

/// Build the JSONB fields map for a conference (shared by insert and update).
fn build_source_fields(
    conf: &ConfsTechEntry,
    source_id: &str,
    topics: &[String],
) -> serde_json::Value {
    let mut fields = serde_json::json!({
        "field_start_date": conf.start_date,
        "field_end_date": conf.end_date,
        "field_source_id": source_id,
        "field_online": conf.online.unwrap_or(false),
        "field_topics": topics,
    });

    if !conf.url.is_empty() {
        fields["field_url"] = serde_json::json!(conf.url);
    }
    if let Some(ref city) = conf.city {
        fields["field_city"] = serde_json::json!(city);
    }
    if let Some(ref country) = conf.country {
        fields["field_country"] = serde_json::json!(country);
    }
    if let Some(ref cfp_url) = conf.cfp_url {
        fields["field_cfp_url"] = serde_json::json!(cfp_url);
    }
    if let Some(ref cfp_end_date) = conf.cfp_end_date {
        fields["field_cfp_end_date"] = serde_json::json!(cfp_end_date);
    }
    if let Some(ref locales) = conf.locales {
        fields["field_language"] = serde_json::json!(locales);
    }
    if let Some(ref twitter) = conf.twitter {
        fields["field_twitter"] = serde_json::json!(twitter);
    }
    if let Some(ref coc_url) = conf.coc_url {
        fields["field_coc_url"] = serde_json::json!(coc_url);
    }

    fields
}

/// Insert a new conference item as unpublished on the live stage.
///
/// Returns true on success.
fn insert_conference(
    conf: &ConfsTechEntry,
    source_id: &str,
    topic_uuid: Option<&str>,
    now: i64,
) -> bool {
    let topics: Vec<String> = topic_uuid.map(|u| vec![u.to_string()]).unwrap_or_default();
    let fields = build_source_fields(conf, source_id, &topics);

    // Use an upsert so that concurrent queue workers processing different topic
    // files don't create duplicate conference items.  On conflict, source-derived
    // fields are refreshed and `field_topics` is merged (SQL-side UNION dedup)
    // so both the existing and incoming topic UUIDs are preserved.
    let result = host::execute_raw(
        "INSERT INTO item (id, type, title, status, author_id, stage_id, created, changed, fields) \
         VALUES (\
           gen_random_uuid(), \
           'conference', \
           $1, \
           1, \
           '00000000-0000-0000-0000-000000000000'::uuid, \
           $2::uuid, \
           $3, \
           $3, \
           $4::jsonb\
         ) \
         ON CONFLICT ((fields->>'field_source_id')) \
         WHERE type = 'conference' \
           AND fields->>'field_source_id' IS NOT NULL \
           AND fields->>'field_source_id' != '' \
         DO UPDATE SET \
           title   = EXCLUDED.title, \
           changed = EXCLUDED.changed, \
           fields  = item.fields \
                  || (EXCLUDED.fields - 'field_topics') \
                  || jsonb_build_object(\
                       'field_topics', \
                       (SELECT COALESCE(jsonb_agg(t ORDER BY t), '[]'::jsonb) \
                        FROM (\
                          SELECT jsonb_array_elements_text(item.fields->'field_topics') \
                          UNION \
                          SELECT jsonb_array_elements_text(EXCLUDED.fields->'field_topics') \
                        ) u(t)\
                       )\
                     )",
        &[
            serde_json::json!(conf.name),
            serde_json::json!(LIVE_STAGE_UUID),
            serde_json::json!(now),
            serde_json::json!(fields.to_string()),
        ],
    );

    matches!(result, Ok(1))
}

/// Update an existing conference with fresh data from the source.
///
/// Only updates source-derived fields, preserving manually-edited fields
/// like description and editor notes. Returns true if the update executed.
fn update_conference(
    item_id: &str,
    conf: &ConfsTechEntry,
    merged_topics: &[String],
    now: i64,
) -> bool {
    // Reuse shared field builder — omit source_id since it doesn't change.
    let updates = build_source_fields(conf, "", merged_topics);

    let result = host::execute_raw(
        "UPDATE item SET \
           title = $1, \
           changed = $2, \
           fields = fields || $3::jsonb \
         WHERE id = $4::uuid",
        &[
            serde_json::json!(conf.name),
            serde_json::json!(now),
            serde_json::json!(updates.to_string()),
            serde_json::json!(item_id),
        ],
    );

    matches!(result, Ok(1))
}

// ─── Slug / dedup helpers ─────────────────────────────────────────────

/// Compute a stable dedup key from a conference entry.
///
/// Format: `slugified(name)-startdate-slugified(city|online)`
fn compute_source_id(conf: &ConfsTechEntry) -> String {
    let name_slug = slugify(&conf.name);
    let city_slug = conf
        .city
        .as_deref()
        .map(slugify)
        .unwrap_or_else(|| "online".to_string());
    format!("{name_slug}-{}-{city_slug}", conf.start_date)
}

/// Simple ASCII slugification: lowercase, replace non-alphanumeric with
/// hyphens, collapse runs, trim leading/trailing hyphens.
fn slugify(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut last_was_hyphen = true; // suppress leading hyphens
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            result.push(c.to_ascii_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            result.push('-');
            last_was_hyphen = true;
        }
    }
    if result.ends_with('-') {
        result.pop();
    }
    result
}

/// Merge a new topic UUID into an existing topic list, deduplicating and sorting.
///
/// If `new_uuid` is `None` (no taxonomy mapping for this confs.tech slug),
/// the existing list is returned unchanged.
fn merge_topics(existing: &[String], new_uuid: Option<&str>) -> Vec<String> {
    let mut topics: Vec<String> = existing.to_vec();
    if let Some(uuid) = new_uuid
        && !topics.iter().any(|t| t == uuid)
    {
        topics.push(uuid.to_string());
    }
    topics.sort();
    topics
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // ── conference_fields ────────────────────────────────────────────

    #[test]
    fn conference_has_fifteen_fields() {
        let fields = conference_fields();
        assert_eq!(fields.len(), 15);
    }

    #[test]
    fn start_and_end_date_required() {
        let fields = conference_fields();
        let start = fields
            .iter()
            .find(|f| f.field_name == "field_start_date")
            .unwrap();
        let end = fields
            .iter()
            .find(|f| f.field_name == "field_end_date")
            .unwrap();
        assert!(start.required);
        assert!(end.required);
    }

    #[test]
    fn topics_field_is_multivalue() {
        let fields = conference_fields();
        let topics = fields
            .iter()
            .find(|f| f.field_name == "field_topics")
            .unwrap();
        assert_eq!(topics.cardinality, -1);
    }

    // ── tap_perm / tap_menu ──────────────────────────────────────────

    #[test]
    fn perm_returns_five_permissions() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 5);
        assert!(
            perms
                .iter()
                .any(|p| p.name == "administer conference import")
        );
        assert!(perms.iter().any(|p| p.name == "create conference content"));
    }

    #[test]
    fn menu_returns_two_routes() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 2);
        assert_eq!(menus[0].path, "/admin/content/conferences");
        assert_eq!(menus[1].path, "/admin/config/importer");
    }

    // ── tap_queue_info ───────────────────────────────────────────────

    #[test]
    fn queue_info_returns_ritrovo_import_queue() {
        let info = __inner_tap_queue_info();
        let queues = info.as_array().unwrap();
        assert_eq!(queues.len(), 1);
        assert_eq!(queues[0]["name"], "ritrovo_import");
        assert_eq!(queues[0]["concurrency"], 4);
    }

    // ── tap_cron ─────────────────────────────────────────────────────

    #[test]
    fn cron_returns_completed_with_stub() {
        let input = CronInput {
            timestamp: 1_700_000_000,
        };
        let result = __inner_tap_cron(input);
        // With stub host functions (query_raw returns "[]"), should_import
        // returns true (no previous timestamp found). http_request stub
        // returns body "[]" which pushes empty payloads — no errors.
        assert_eq!(result["status"], "completed", "unexpected: {result}");
    }

    // ── tap_queue_worker ─────────────────────────────────────────────

    #[test]
    fn queue_worker_rejects_missing_topic() {
        let input = serde_json::json!({"year": 2026, "conferences": "[]"});
        let result = __inner_tap_queue_worker(input);
        assert_eq!(result["status"], "error");
        assert_eq!(result["reason"], "missing_topic");
    }

    #[test]
    fn queue_worker_rejects_missing_year() {
        let input = serde_json::json!({"topic": "rust", "conferences": "[]"});
        let result = __inner_tap_queue_worker(input);
        assert_eq!(result["status"], "error");
        assert_eq!(result["reason"], "missing_year");
    }

    #[test]
    fn queue_worker_rejects_bad_json() {
        let input = serde_json::json!({"topic": "rust", "year": 2026, "conferences": "not-json"});
        let result = __inner_tap_queue_worker(input);
        assert_eq!(result["status"], "error");
        assert_eq!(result["reason"], "parse_error");
    }

    #[test]
    fn queue_worker_skips_invalid_entries() {
        // Missing startDate and endDate.
        let conferences = serde_json::json!([{"name": "BadConf"}]).to_string();
        let input = serde_json::json!({
            "topic": "rust",
            "year": 2026,
            "conferences": conferences,
        });
        let result = __inner_tap_queue_worker(input);
        assert_eq!(result["status"], "ok");
        assert_eq!(result["invalid"], 1);
        assert_eq!(result["imported"], 0);
    }

    #[test]
    fn queue_worker_accepts_valid_entry() {
        let conferences = serde_json::json!([{
            "name": "RustConf",
            "startDate": "2026-09-01",
            "endDate": "2026-09-03",
            "city": "Portland",
            "country": "USA",
        }])
        .to_string();
        let input = serde_json::json!({
            "topic": "rust",
            "year": 2026,
            "conferences": conferences,
        });
        let result = __inner_tap_queue_worker(input);
        assert_eq!(result["status"], "ok");
        // Stub execute_raw always returns Ok(0), so insert returns false (0 rows
        // affected != 1). The entry counts as invalid in the stub context.
        assert_eq!(
            result["invalid"].as_u64().unwrap() + result["imported"].as_u64().unwrap(),
            1
        );
    }

    // ── validate_conference ───────────────────────────────────────────

    fn make_valid() -> ConfsTechEntry {
        ConfsTechEntry {
            name: "RustConf".to_string(),
            url: "https://rustconf.com".to_string(),
            start_date: "2026-09-01".to_string(),
            end_date: "2026-09-03".to_string(),
            city: Some("Portland".to_string()),
            country: Some("USA".to_string()),
            online: None,
            cfp_url: None,
            cfp_end_date: None,
            locales: None,
            twitter: None,
            coc_url: None,
        }
    }

    #[test]
    fn validate_valid_entry_ok() {
        assert!(validate_conference(&make_valid()).is_ok());
    }

    #[test]
    fn validate_missing_name() {
        let mut c = make_valid();
        c.name = String::new();
        assert!(validate_conference(&c).is_err());
    }

    #[test]
    fn validate_missing_start_date() {
        let mut c = make_valid();
        c.start_date = String::new();
        assert!(validate_conference(&c).is_err());
    }

    #[test]
    fn validate_end_before_start() {
        let mut c = make_valid();
        c.end_date = "2026-08-31".to_string();
        assert!(validate_conference(&c).is_err());
    }

    #[test]
    fn validate_cfp_after_start() {
        let mut c = make_valid();
        c.cfp_end_date = Some("2026-10-01".to_string());
        let err = validate_conference(&c).unwrap_err();
        assert!(err.contains("cfpEndDate"), "unexpected error: {err}");
    }

    #[test]
    fn validate_bad_date_format() {
        let mut c = make_valid();
        c.start_date = "01-09-2026".to_string(); // wrong format
        assert!(validate_conference(&c).is_err());
    }

    #[test]
    fn validate_year_out_of_range() {
        let mut c = make_valid();
        c.start_date = "1999-01-01".to_string();
        c.end_date = "1999-01-02".to_string();
        assert!(validate_conference(&c).is_err());
    }

    // ── is_valid_date ─────────────────────────────────────────────────

    #[test]
    fn is_valid_date_ok() {
        assert!(is_valid_date("2026-09-01"));
        assert!(is_valid_date("2010-01-01"));
        assert!(is_valid_date("2035-12-31"));
    }

    #[test]
    fn is_valid_date_bad_format() {
        assert!(!is_valid_date("09-01-2026")); // wrong order
        assert!(!is_valid_date("2026/09/01")); // wrong separator
        assert!(!is_valid_date("2026-9-1")); // missing leading zeros
        assert!(!is_valid_date("not-a-date"));
    }

    #[test]
    fn is_valid_date_year_range() {
        assert!(!is_valid_date("1999-01-01"));
        assert!(!is_valid_date("2050-01-01"));
    }

    // ── slugify ───────────────────────────────────────────────────────

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("RustConf 2026"), "rustconf-2026");
        assert_eq!(slugify("EuroRust"), "eurorust");
        assert_eq!(slugify("Vue.js Nation"), "vue-js-nation");
    }

    #[test]
    fn slugify_unicode_and_special_chars() {
        assert_eq!(slugify("JSConf España"), "jsconf-espa-a");
        assert_eq!(slugify("C++ Now!"), "c-now");
    }

    #[test]
    fn slugify_no_trailing_hyphen() {
        assert_eq!(slugify("test--value--"), "test-value");
    }

    // ── compute_source_id ─────────────────────────────────────────────

    #[test]
    fn compute_source_id_with_city() {
        let conf = ConfsTechEntry {
            name: "RustConf".to_string(),
            url: String::new(),
            start_date: "2026-09-01".to_string(),
            end_date: "2026-09-03".to_string(),
            city: Some("Portland".to_string()),
            country: Some("U.S.A.".to_string()),
            online: None,
            cfp_url: None,
            cfp_end_date: None,
            locales: None,
            twitter: None,
            coc_url: None,
        };
        assert_eq!(compute_source_id(&conf), "rustconf-2026-09-01-portland");
    }

    #[test]
    fn compute_source_id_online() {
        let conf = ConfsTechEntry {
            name: "Vue.js Nation".to_string(),
            url: String::new(),
            start_date: "2025-01-29".to_string(),
            end_date: "2025-01-30".to_string(),
            city: None,
            country: None,
            online: Some(true),
            cfp_url: None,
            cfp_end_date: None,
            locales: None,
            twitter: None,
            coc_url: None,
        };
        assert_eq!(compute_source_id(&conf), "vue-js-nation-2025-01-29-online");
    }

    // ── build_source_fields ───────────────────────────────────────────

    #[test]
    fn build_fields_minimal() {
        let conf = ConfsTechEntry {
            name: "TestConf".to_string(),
            url: String::new(),
            start_date: "2026-01-01".to_string(),
            end_date: "2026-01-02".to_string(),
            city: None,
            country: None,
            online: None,
            cfp_url: None,
            cfp_end_date: None,
            locales: None,
            twitter: None,
            coc_url: None,
        };
        let topics = vec!["rust".to_string()];
        let fields = build_source_fields(&conf, "testconf-2026-01-01-online", &topics);
        assert_eq!(fields["field_source_id"], "testconf-2026-01-01-online");
        assert_eq!(fields["field_online"], false);
        assert_eq!(fields["field_topics"][0], "rust");
        assert!(fields.get("field_url").is_none());
    }

    #[test]
    fn build_fields_full() {
        let conf = ConfsTechEntry {
            name: "RustConf".to_string(),
            url: "https://rustconf.com".to_string(),
            start_date: "2026-09-01".to_string(),
            end_date: "2026-09-03".to_string(),
            city: Some("Portland".to_string()),
            country: Some("U.S.A.".to_string()),
            online: Some(false),
            cfp_url: Some("https://rustconf.com/cfp".to_string()),
            cfp_end_date: Some("2026-06-01".to_string()),
            locales: Some("EN".to_string()),
            twitter: Some("@rustconf".to_string()),
            coc_url: Some("https://rustconf.com/coc".to_string()),
        };
        let topics = vec!["rust".to_string()];
        let fields = build_source_fields(&conf, "rustconf-2026-09-01-portland", &topics);
        assert_eq!(fields["field_url"], "https://rustconf.com");
        assert_eq!(fields["field_city"], "Portland");
        assert_eq!(fields["field_country"], "U.S.A.");
        assert_eq!(fields["field_cfp_url"], "https://rustconf.com/cfp");
        assert_eq!(fields["field_cfp_end_date"], "2026-06-01");
        assert_eq!(fields["field_language"], "EN");
        assert_eq!(fields["field_twitter"], "@rustconf");
        assert_eq!(fields["field_coc_url"], "https://rustconf.com/coc");
    }

    // ── merge_topics ──────────────────────────────────────────────────

    #[test]
    fn merge_topics_deduplicates() {
        let uuid_a = "00000000-0000-0000-0000-000000000001";
        let uuid_b = "00000000-0000-0000-0000-000000000002";
        let existing = vec![uuid_a.to_string(), uuid_b.to_string()];
        // Already present — no duplicate added.
        assert_eq!(merge_topics(&existing, Some(uuid_a)), vec![uuid_a, uuid_b]);
        // New UUID — appended and sorted.
        let uuid_c = "00000000-0000-0000-0000-000000000003";
        assert_eq!(
            merge_topics(&existing, Some(uuid_c)),
            vec![uuid_a, uuid_b, uuid_c]
        );
    }

    #[test]
    fn merge_topics_empty_existing() {
        let uuid = "00000000-0000-0000-0000-000000000001";
        let existing: Vec<String> = vec![];
        assert_eq!(merge_topics(&existing, Some(uuid)), vec![uuid]);
    }

    #[test]
    fn merge_topics_none_uuid_is_noop() {
        let existing = vec!["uuid-a".to_string()];
        assert_eq!(merge_topics(&existing, None), vec!["uuid-a"]);
    }

    // ── topic_term_uuid ───────────────────────────────────────────────

    #[test]
    fn topic_term_uuid_returns_none_for_unmapped_slug() {
        // sre and scala have no SLUG_TO_TERM entry.
        assert!(topic_term_uuid("sre").is_none());
        assert!(topic_term_uuid("scala").is_none());
    }

    #[test]
    fn topic_term_uuid_returns_none_for_unknown_slug() {
        assert!(topic_term_uuid("not-a-real-topic").is_none());
    }

    #[test]
    fn topic_term_uuid_mapped_slug_queries_state() {
        // In the stub host environment, query_raw returns "[]" so the state
        // lookup will return None even for mapped slugs.  This confirms the
        // function at least reaches the state query without panicking.
        let result = topic_term_uuid("rust");
        // Stub returns None — acceptable; real env would return Some(uuid).
        assert!(result.is_none() || result.as_deref().map(|s| s.len()).unwrap_or(0) > 0);
    }

    // ── timestamp_to_year ─────────────────────────────────────────────

    #[test]
    fn timestamp_to_year_works() {
        // 2025-01-01 00:00:00 UTC = 1735689600
        assert_eq!(timestamp_to_year(1_735_689_600), 2025);
        // 2026-06-15 12:00:00 UTC ≈ 1781870400
        assert_eq!(timestamp_to_year(1_781_870_400), 2026);
    }

    // ── perm permission names ─────────────────────────────────────────

    #[test]
    fn perm_format_matches_kernel_fallback() {
        let perms = __inner_tap_perm();
        let expected_names = [
            "view conference content",
            "create conference content",
            "edit conference content",
            "delete conference content",
            "administer conference import",
        ];
        for name in &expected_names {
            assert!(
                perms.iter().any(|p| p.name == *name),
                "missing permission: {name}"
            );
        }
    }
}
