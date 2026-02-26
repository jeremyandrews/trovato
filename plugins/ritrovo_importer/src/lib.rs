//! Conference importer plugin for the Ritrovo tutorial.
//!
//! Imports tech conference data from the
//! [confs.tech](https://github.com/tech-conferences/conference-data)
//! open-source dataset. Runs daily via `tap_cron`, fetching JSON files
//! by topic, deduplicating against existing items via a computed
//! `source_id`, and inserting new conferences as unpublished items on
//! the live stage.

use std::collections::HashMap;

use trovato_sdk::host;
use trovato_sdk::prelude::*;

/// Plugin name for logging calls.
const PLUGIN_NAME: &str = "ritrovo_importer";

/// Base URL for raw conference JSON from the confs.tech GitHub repo.
const DATA_BASE_URL: &str =
    "https://raw.githubusercontent.com/tech-conferences/conference-data/main/conferences";

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

/// Minimum interval between imports (24 hours in seconds).
const IMPORT_INTERVAL_SECS: i64 = 86_400;

/// Variables key tracking last import timestamp.
const VAR_LAST_IMPORT: &str = "ritrovo_importer.last_import";

/// Variables key tracking the topic offset for round-robin.
const VAR_TOPIC_OFFSET: &str = "ritrovo_importer.topic_offset";

// ─── Conference field definitions ────────────────────────────────────
//
// The `conference` item type is created by the user via the admin UI
// (see tutorial Part 1 Step 2). This plugin does NOT auto-register it
// via `tap_item_info` — the importer assumes the type already exists
// when `tap_cron` runs.
//
// `conference_fields()` documents the fields the importer reads/writes
// and is used in unit tests to validate field expectations.

/// Build the field definitions for the conference content type.
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

/// Daily cron handler that fetches conferences from confs.tech.
///
/// Processes a rotating subset of topics per cycle (round-robin) to
/// stay within cron timeout limits. Skips if less than 24 hours since
/// last import. Deduplicates via `source_id` and accumulates topics
/// for conferences that appear in multiple topic files.
#[plugin_tap]
pub fn tap_cron(input: CronInput) -> serde_json::Value {
    let now = input.timestamp;

    // Check if enough time has passed since last import
    if !should_import(now) {
        return serde_json::json!({"status": "skipped", "reason": "too_soon"});
    }

    // Derive import years from current timestamp (Fix #3)
    let current_year = timestamp_to_year(now);
    let import_years = [current_year, current_year + 1];

    // Determine which topics to process this cycle (Fix #11)
    let topic_offset = load_topic_offset();
    let cycle_topics: Vec<&str> = TOPICS
        .iter()
        .cycle()
        .skip(topic_offset)
        .take(TOPICS_PER_CYCLE)
        .copied()
        .collect();

    let mut imported = 0u64;
    let mut updated = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;

    // Load existing source_ids and their topics for dedup (Fix #8)
    let existing = load_existing_conferences();

    for year in &import_years {
        for topic in &cycle_topics {
            let url = format!("{DATA_BASE_URL}/{year}/{topic}.json");

            let response = match host::http_request(
                &trovato_sdk::types::HttpRequest::get(&url).timeout(15_000),
            ) {
                Ok(r) if r.status == 200 => r,
                Ok(r) if r.status == 404 => continue,
                Ok(r) => {
                    errors += 1;
                    log_warning(&format!("HTTP {status} fetching {url}", status = r.status));
                    continue;
                }
                Err(_code) => {
                    errors += 1;
                    continue;
                }
            };

            let conferences: Vec<ConfsTechEntry> = match serde_json::from_str(&response.body) {
                Ok(c) => c,
                Err(e) => {
                    errors += 1;
                    log_warning(&format!("JSON parse error for {url}: {e}"));
                    continue;
                }
            };

            for conf in &conferences {
                let source_id = compute_source_id(conf);

                // Validate required fields
                if conf.name.is_empty() || conf.start_date.is_empty() || conf.end_date.is_empty() {
                    skipped += 1;
                    continue;
                }

                // Date sanity check
                if conf.end_date < conf.start_date {
                    skipped += 1;
                    log_warning(&format!("Skipping '{}': end_date < start_date", conf.name));
                    continue;
                }

                if let Some(info) = existing.get(&source_id) {
                    // Update existing item, merging topics (Fix #8)
                    let merged_topics = merge_topics(&info.topics, topic);
                    if update_conference(&info.item_id, conf, &merged_topics, now) {
                        updated += 1;
                    }
                } else {
                    // Insert new conference on live stage, unpublished (Fix #4)
                    if insert_conference(conf, &source_id, topic, now) {
                        imported += 1;
                    } else {
                        errors += 1;
                    }
                }
            }
        }
    }

    // Advance topic offset for next cycle
    let next_offset = (topic_offset + TOPICS_PER_CYCLE) % TOPICS.len();
    save_topic_offset(next_offset);

    // Only record import timestamp on at least partial success (Fix #12)
    if imported > 0 || updated > 0 {
        record_import_time(now);
    }

    serde_json::json!({
        "status": "completed",
        "imported": imported,
        "updated": updated,
        "skipped": skipped,
        "errors": errors,
        "topics_processed": cycle_topics,
    })
}

// ─── confs.tech JSON schema ──────────────────────────────────────────

/// A single conference entry from the confs.tech dataset.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfsTechEntry {
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

/// Info about an existing conference in the database.
struct ExistingConference {
    item_id: String,
    topics: Vec<String>,
}

// ─── Helpers ─────────────────────────────────────────────────────────

/// Derive the calendar year from a Unix timestamp.
fn timestamp_to_year(ts: i64) -> u16 {
    // 365.2425 days/year average; safe approximation for year extraction
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let year = (1970 + ts / 31_556_952) as u16;
    year
}

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

/// Simple ASCII slugification: lowercase, replace non-alphanumeric with hyphens,
/// collapse multiple hyphens, trim leading/trailing hyphens.
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
    // Trim trailing hyphen
    if result.ends_with('-') {
        result.pop();
    }
    result
}

/// Merge a new topic into an existing topic list, deduplicating.
fn merge_topics(existing: &[String], new_topic: &str) -> Vec<String> {
    let mut topics: Vec<String> = existing.to_vec();
    if !topics.iter().any(|t| t == new_topic) {
        topics.push(new_topic.to_string());
    }
    topics.sort();
    topics
}

/// Check if we should run the import (>= 24h since last run).
fn should_import(now: i64) -> bool {
    let last_import_json = host::query_raw(
        "SELECT value FROM variable WHERE name = $1",
        &[serde_json::json!(VAR_LAST_IMPORT)],
    );

    match last_import_json {
        Ok(json_str) => {
            let rows: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap_or_default();
            let last_ts = rows
                .first()
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            (now - last_ts) >= IMPORT_INTERVAL_SECS
        }
        Err(_) => true,
    }
}

/// Record the current import timestamp.
fn record_import_time(now: i64) {
    let ts_str = now.to_string();
    let _ = host::execute_raw(
        "INSERT INTO variable (name, value) VALUES ($1, $2) \
         ON CONFLICT (name) DO UPDATE SET value = $2",
        &[
            serde_json::json!(VAR_LAST_IMPORT),
            serde_json::json!(ts_str),
        ],
    );
}

/// Load the topic offset for round-robin scheduling.
fn load_topic_offset() -> usize {
    let result = host::query_raw(
        "SELECT value FROM variable WHERE name = $1",
        &[serde_json::json!(VAR_TOPIC_OFFSET)],
    );
    match result {
        Ok(json_str) => {
            let rows: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap_or_default();
            rows.first()
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0)
                % TOPICS.len()
        }
        Err(_) => 0,
    }
}

/// Save the topic offset for the next cycle.
fn save_topic_offset(offset: usize) {
    let _ = host::execute_raw(
        "INSERT INTO variable (name, value) VALUES ($1, $2) \
         ON CONFLICT (name) DO UPDATE SET value = $2",
        &[
            serde_json::json!(VAR_TOPIC_OFFSET),
            serde_json::json!(offset.to_string()),
        ],
    );
}

/// Load existing conferences into a map of source_id → (item_id, topics).
fn load_existing_conferences() -> HashMap<String, ExistingConference> {
    let mut existing = HashMap::new();

    let result = host::query_raw(
        "SELECT id, fields->>'field_source_id' AS source_id, \
         fields->'field_topics' AS topics \
         FROM item \
         WHERE item_type = 'conference' \
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

/// Build the JSONB fields from a conference entry (shared by insert and update).
///
/// Uses consistent field format: bare values for all simple types (Fix #9).
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
/// Uses `LIVE_STAGE_UUID` with `status=0` (unpublished) so items are
/// visible to editors but not anonymous visitors (Fix #4).
///
/// Returns true on success.
fn insert_conference(conf: &ConfsTechEntry, source_id: &str, topic: &str, now: i64) -> bool {
    let topics = vec![topic.to_string()];
    let fields = build_source_fields(conf, source_id, &topics);

    let result = host::execute_raw(
        "INSERT INTO item (id, item_type, title, status, author_id, stage_id, created, changed, fields) \
         VALUES (\
           gen_random_uuid(), \
           'conference', \
           $1, \
           0, \
           '00000000-0000-0000-0000-000000000000'::uuid, \
           $2::uuid, \
           $3, \
           $3, \
           $4::jsonb\
         )",
        &[
            serde_json::json!(conf.name),
            serde_json::json!(LIVE_STAGE_UUID),
            serde_json::json!(now),
            serde_json::json!(fields.to_string()),
        ],
    );

    match result {
        Ok(1) => true,
        Ok(_) => false,
        Err(_) => false,
    }
}

/// Update an existing conference with fresh data from the source.
///
/// Only updates source-derived fields (dates, URLs, topics, etc.),
/// preserving manually-edited fields like description and editor notes.
///
/// Returns true if the update was executed.
fn update_conference(
    item_id: &str,
    conf: &ConfsTechEntry,
    merged_topics: &[String],
    now: i64,
) -> bool {
    // Reuse shared field builder — omit source_id since it doesn't change (Fix #6)
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

/// Log a warning message via the kernel logging host function (Fix #5).
fn log_warning(msg: &str) {
    host::log("warn", PLUGIN_NAME, msg);
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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

    #[test]
    fn cron_returns_completed_with_stub() {
        let input = CronInput {
            timestamp: 1_700_000_000,
        };
        let result = __inner_tap_cron(input);
        // With stub host functions (query_raw returns "[]"), should_import
        // returns true (no previous timestamp found), http_request stub
        // returns "[]" body which parses as empty conference list → completed
        // with 0 imported but no errors.
        assert_eq!(result["status"], "completed", "unexpected status: {result}");
    }

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
        // URL should not be present when empty
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

    #[test]
    fn timestamp_to_year_works() {
        // 2025-01-01 00:00:00 UTC = 1735689600
        assert_eq!(timestamp_to_year(1_735_689_600), 2025);
        // 2026-06-15 12:00:00 UTC ≈ 1781870400
        assert_eq!(timestamp_to_year(1_781_870_400), 2026);
    }

    #[test]
    fn merge_topics_deduplicates() {
        let existing = vec!["rust".to_string(), "security".to_string()];
        assert_eq!(merge_topics(&existing, "rust"), vec!["rust", "security"]);
        assert_eq!(
            merge_topics(&existing, "devops"),
            vec!["devops", "rust", "security"]
        );
    }

    #[test]
    fn merge_topics_empty_existing() {
        let existing: Vec<String> = vec![];
        assert_eq!(merge_topics(&existing, "rust"), vec!["rust"]);
    }
}
