//! Gather access enforcement — pure helpers and tuning constants (Story 3.4).
//!
//! The gather read path streams **projected `row_to_json` rows**, not hydrated
//! `Item` structs, so it cannot drop the shared [`ItemService`] seam
//! ([`filter_page_for_view`]) in mechanically. This module bridges the gap:
//!
//! - [`access_item_from_row`] reconstructs the *access-relevant* slice of an
//!   `Item` (id / type / status / author_id / stage_id) from a projected row so
//!   the authoritative post-fetch `check_access` pass can run on candidates. The
//!   query builder guarantees those columns are always projected (see
//!   `add_access_columns`).
//! - [`field_projection_map`] maps a gather's explicit `fields.*` projections
//!   back to the item field name the field-access decision is keyed on, so
//!   restricted fields can be dropped from the projected columns.
//! - The tuning constants back the D-26 over-fetch/geometric-backfill loop
//!   (`GatherService::execute_definition_with_stages`).
//!
//! [`ItemService`]: crate::content::ItemService
//! [`filter_page_for_view`]: crate::content::ItemService::filter_page_for_view

use crate::gather::types::QueryField;
use crate::models::Item;
use crate::models::stage::LIVE_STAGE_ID;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Columns the query builder always projects so the item-access pass can run on
/// candidate rows, even for gathers that otherwise project an explicit field
/// list. `type` is the item-type column (`Item`'s `#[sqlx(rename = "type")]`).
pub(crate) const ACCESS_COLUMNS: [&str; 5] = ["id", "type", "status", "author_id", "stage_id"];

/// Tuning for the D-26 over-fetch / geometric-backfill loop.
///
/// These were `LazyLock` statics reading the environment on first use, which made
/// them un-steerable from a test without mutating a process-global — and once
/// resolved, un-steerable at all for the life of the process, so no two gather
/// tests could exercise different bounds. Resolved once by `Config::from_env` and
/// carried on `GatherService`, they are ordinary inputs.
///
/// Every field is required to be positive: a zero fetch factor cannot return a
/// row, a zero scan or round cap cannot terminate usefully, so a non-positive
/// configured value falls back to the documented default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GatherAccessConfig {
    /// Initial over-fetch window multiplier: the first backfill round fetches
    /// `page_size × fetch_factor` candidates. `GATHER_ACCESS_FETCH_FACTOR`,
    /// default 2.
    pub fetch_factor: u32,

    /// Hard cap on candidate rows examined by the over-fetch loop for one page —
    /// the deliberate request-amplification/DoS bound (D-26).
    /// `GATHER_ACCESS_MAX_SCAN`, default 1000.
    pub max_scan: u32,

    /// Hard cap on backfill rounds (a second termination guard alongside the
    /// scan cap). `GATHER_ACCESS_MAX_ROUNDS`, default 6.
    pub max_backfill_rounds: u32,

    /// Upper bound on the pgvector semantic candidate pool once access filtering
    /// is active — raised from the historical top-100 so restricted viewers are
    /// not starved, but bounded (exact cosine cost grows with the pool).
    /// `GATHER_SEMANTIC_SEARCH_MAX`, default 500.
    pub semantic_search_max: u32,
}

impl Default for GatherAccessConfig {
    fn default() -> Self {
        Self {
            fetch_factor: 2,
            max_scan: 1000,
            max_backfill_rounds: 6,
            semantic_search_max: 500,
        }
    }
}

impl GatherAccessConfig {
    /// Resolve the tuning from a settings lookup, as documented per field.
    pub(crate) fn from_lookup(lookup: crate::config::Lookup<'_>) -> Self {
        let defaults = Self::default();
        Self {
            fetch_factor: crate::config::parse_positive_or(
                lookup,
                "GATHER_ACCESS_FETCH_FACTOR",
                defaults.fetch_factor,
            ),
            max_scan: crate::config::parse_positive_or(
                lookup,
                "GATHER_ACCESS_MAX_SCAN",
                defaults.max_scan,
            ),
            max_backfill_rounds: crate::config::parse_positive_or(
                lookup,
                "GATHER_ACCESS_MAX_ROUNDS",
                defaults.max_backfill_rounds,
            ),
            semantic_search_max: crate::config::parse_positive_or(
                lookup,
                "GATHER_SEMANTIC_SEARCH_MAX",
                defaults.semantic_search_max,
            ),
        }
    }
}

fn parse_uuid(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<Uuid> {
    obj.get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// Reconstruct the access-relevant slice of an [`Item`] from a projected gather
/// row (`row_to_json` output). Only the fields [`crate::content::ItemService::check_access`]
/// reads are populated (id, type, author_id, status, stage_id); the rest carry
/// harmless defaults. Returns `None` if the row lacks a parseable `id` or `type`
/// — such a row cannot be access-checked and the caller must drop it (deny).
pub(crate) fn access_item_from_row(row: &serde_json::Value) -> Option<Item> {
    let obj = row.as_object()?;
    let id = parse_uuid(obj, "id")?;
    let item_type = obj.get("type").and_then(|v| v.as_str())?.to_string();
    // author_id / stage_id / status should always be present (always projected);
    // fall back conservatively if a custom base table omits them.
    let author_id = parse_uuid(obj, "author_id").unwrap_or_else(Uuid::nil);
    let stage_id = parse_uuid(obj, "stage_id").unwrap_or(LIVE_STAGE_ID);
    let status = obj
        .get("status")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        .clamp(i16::MIN as i64, i16::MAX as i64) as i16;

    Some(Item {
        id,
        current_revision_id: None,
        item_type,
        title: String::new(),
        author_id,
        status,
        created: 0,
        changed: 0,
        promote: 0,
        sticky: 0,
        fields: serde_json::Value::Null,
        stage_id,
        language: "en".to_string(),
        item_group_id: Uuid::nil(),
        retention_days: None,
    })
}

/// For a gather that projects an **explicit** field list, map each output key
/// that comes from a dynamic item field (`fields.<name>` projections) to the
/// item field name the field-access decision is keyed on. Raw column
/// projections (e.g. `title`, `status`) are not dynamic fields and are omitted
/// (they are never governed by `tap_field_access`).
///
/// The output key is the projection's label when set, else the JSONB path — the
/// same aliasing the query builder's `add_select_fields` applies. A nested path
/// (`fields.meta.sub`) is governed by its **top-level** field (`meta`), since
/// field access is per top-level field.
pub(crate) fn field_projection_map(fields: &[QueryField]) -> Vec<(String, String)> {
    let mut map = Vec::new();
    for field in fields {
        if let Some(path) = field.field_name.strip_prefix("fields.") {
            let output_key = field.label.clone().unwrap_or_else(|| path.to_string());
            let item_field = path.split('.').next().unwrap_or(path).to_string();
            map.push((output_key, item_field));
        }
    }
    map
}

/// Drop the fields the viewer may not see from one projected gather row
/// (Story 3.4 tier-2), given the per-type field-access `decisions` (a `false`
/// value = deny). Pure so the drop mechanics are unit-tested without a DB or a
/// denying plugin (the decision itself is validated end-to-end in Story 3.8).
///
/// - `SELECT item.*` rows (`is_star`): denied keys are removed from the row's
///   nested `fields` object; every other key is untouched.
/// - Explicit-field rows: the row is rebuilt to only its originally-requested
///   `output_keys`, minus any whose backing dynamic field was denied — which
///   also strips the access columns injected for the item-access pass.
pub(crate) fn filter_row_fields(
    row: &mut serde_json::Value,
    is_star: bool,
    field_map: &[(String, String)],
    output_keys: &[String],
    decisions: Option<&HashMap<String, bool>>,
) {
    if is_star {
        if let Some(fields) = row.get_mut("fields").and_then(|v| v.as_object_mut())
            && let Some(decisions) = decisions
        {
            fields.retain(|k, _| decisions.get(k).copied().unwrap_or(true));
        }
    } else if let Some(obj) = row.as_object_mut() {
        let denied: HashSet<&str> = field_map
            .iter()
            .filter(|(_out, item_field)| {
                decisions
                    .and_then(|d| d.get(item_field))
                    .copied()
                    .map(|allowed| !allowed)
                    .unwrap_or(false)
            })
            .map(|(out, _)| out.as_str())
            .collect();
        obj.retain(|k, _| output_keys.iter().any(|o| o == k) && !denied.contains(k.as_str()));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::gather::types::QueryField;

    /// Resolve the access tuning from an explicit settings map. Nothing global
    /// is involved, which is what the `LazyLock` statics made impossible.
    fn access_from_map(pairs: &[(&str, &str)]) -> GatherAccessConfig {
        let settings: HashMap<&str, &str> = pairs.iter().copied().collect();
        GatherAccessConfig::from_lookup(&|name| settings.get(name).map(|v| (*v).to_string()))
    }

    #[test]
    fn access_config_defaults_when_nothing_is_configured() {
        assert_eq!(access_from_map(&[]), GatherAccessConfig::default());
        let defaults = GatherAccessConfig::default();
        assert_eq!(defaults.fetch_factor, 2);
        assert_eq!(defaults.max_scan, 1000);
        assert_eq!(defaults.max_backfill_rounds, 6);
        assert_eq!(defaults.semantic_search_max, 500);
    }

    #[test]
    fn access_config_reads_every_documented_setting() {
        let config = access_from_map(&[
            ("GATHER_ACCESS_FETCH_FACTOR", "3"),
            ("GATHER_ACCESS_MAX_SCAN", "250"),
            ("GATHER_ACCESS_MAX_ROUNDS", "4"),
            ("GATHER_SEMANTIC_SEARCH_MAX", "120"),
        ]);
        assert_eq!(config.fetch_factor, 3);
        assert_eq!(config.max_scan, 250);
        assert_eq!(config.max_backfill_rounds, 4);
        assert_eq!(config.semantic_search_max, 120);
    }

    /// Zero, negative and unparseable all fall back. Zero matters most: a zero
    /// fetch factor makes the over-fetch window zero rows wide, so the backfill
    /// loop could never return a page.
    #[test]
    fn access_config_rejects_non_positive_values() {
        for bad in ["0", "-1", "", "lots", "2.5"] {
            let config = access_from_map(&[
                ("GATHER_ACCESS_FETCH_FACTOR", bad),
                ("GATHER_ACCESS_MAX_SCAN", bad),
                ("GATHER_ACCESS_MAX_ROUNDS", bad),
                ("GATHER_SEMANTIC_SEARCH_MAX", bad),
            ]);
            assert_eq!(
                config,
                GatherAccessConfig::default(),
                "{bad:?} must fall back to the documented defaults"
            );
        }
    }

    #[test]
    fn access_item_reconstructs_from_full_row() {
        let id = Uuid::now_v7();
        let author = Uuid::now_v7();
        let stage = Uuid::now_v7();
        let row = serde_json::json!({
            "id": id.to_string(),
            "type": "conference",
            "title": "X",
            "author_id": author.to_string(),
            "status": 1,
            "stage_id": stage.to_string(),
            "fields": { "field_city": { "value": "Barga" } },
        });
        let item = access_item_from_row(&row).unwrap();
        assert_eq!(item.id, id);
        assert_eq!(item.item_type, "conference");
        assert_eq!(item.author_id, author);
        assert_eq!(item.status, 1);
        assert_eq!(item.stage_id, stage);
        assert!(item.is_published());
    }

    #[test]
    fn access_item_none_without_id_or_type() {
        assert!(access_item_from_row(&serde_json::json!({ "type": "x" })).is_none());
        assert!(
            access_item_from_row(&serde_json::json!({ "id": Uuid::now_v7().to_string() }))
                .is_none()
        );
        assert!(access_item_from_row(&serde_json::json!("not an object")).is_none());
    }

    #[test]
    fn access_item_defaults_missing_access_columns() {
        // A custom base table may omit author_id/stage_id/status.
        let row = serde_json::json!({ "id": Uuid::now_v7().to_string(), "type": "page" });
        let item = access_item_from_row(&row).unwrap();
        assert_eq!(item.author_id, Uuid::nil());
        assert_eq!(item.stage_id, LIVE_STAGE_ID);
        assert_eq!(item.status, 0);
        assert!(!item.is_published());
    }

    fn field(name: &str, label: Option<&str>) -> QueryField {
        QueryField {
            field_name: name.to_string(),
            table_alias: None,
            label: label.map(str::to_string),
        }
    }

    #[test]
    fn filter_row_star_drops_denied_fields_only() {
        let mut row = serde_json::json!({
            "id": "x", "type": "person",
            "fields": { "name": "Ada", "ssn": "123", "salary": "9" },
        });
        let mut decisions = HashMap::new();
        decisions.insert("ssn".to_string(), false); // deny
        decisions.insert("salary".to_string(), true); // allow
        // "name" absent from decisions ⇒ fail-open keep.
        filter_row_fields(&mut row, true, &[], &[], Some(&decisions));
        let fields = row.get("fields").unwrap().as_object().unwrap();
        assert!(!fields.contains_key("ssn"), "denied field must be dropped");
        assert!(fields.contains_key("salary"));
        assert!(
            fields.contains_key("name"),
            "un-opinioned field kept (fail-open)"
        );
        // Non-field keys untouched.
        assert_eq!(row.get("type").unwrap(), "person");
    }

    #[test]
    fn filter_row_explicit_drops_denied_and_strips_access_columns() {
        // Row as projected: requested "title" + "ssn" (from fields.ssn) plus the
        // injected access columns (id/type/status/author_id/stage_id).
        let mut row = serde_json::json!({
            "title": "T",
            "ssn": "123",
            "id": "abc", "type": "person", "status": 1,
            "author_id": "a", "stage_id": "s",
        });
        let field_map = vec![("ssn".to_string(), "ssn".to_string())];
        let output_keys = vec!["title".to_string(), "ssn".to_string()];
        let mut decisions = HashMap::new();
        decisions.insert("ssn".to_string(), false); // deny ssn
        filter_row_fields(&mut row, false, &field_map, &output_keys, Some(&decisions));
        let obj = row.as_object().unwrap();
        assert!(obj.contains_key("title"), "requested non-field column kept");
        assert!(!obj.contains_key("ssn"), "denied projected field dropped");
        // Injected access columns are stripped from the output.
        for col in ["id", "type", "status", "author_id", "stage_id"] {
            assert!(
                !obj.contains_key(col),
                "access column {col} must be stripped"
            );
        }
    }

    #[test]
    fn filter_row_explicit_keeps_allowed_field() {
        let mut row = serde_json::json!({
            "ssn": "123", "id": "abc", "type": "person",
        });
        let field_map = vec![("ssn".to_string(), "ssn".to_string())];
        let output_keys = vec!["ssn".to_string()];
        // No decision / allow ⇒ kept; access columns still stripped.
        filter_row_fields(&mut row, false, &field_map, &output_keys, None);
        let obj = row.as_object().unwrap();
        assert!(obj.contains_key("ssn"));
        assert!(!obj.contains_key("id"));
        assert!(!obj.contains_key("type"));
    }

    #[test]
    fn field_projection_map_only_dynamic_fields() {
        let fields = vec![
            field("title", None),                // raw column — omitted
            field("fields.ssn", None),           // dynamic — key "ssn" -> "ssn"
            field("fields.salary", Some("pay")), // labeled — key "pay" -> "salary"
            field("fields.meta.sub", Some("m")), // nested — key "m" -> "meta"
        ];
        let map = field_projection_map(&fields);
        assert_eq!(
            map,
            vec![
                ("ssn".to_string(), "ssn".to_string()),
                ("pay".to_string(), "salary".to_string()),
                ("m".to_string(), "meta".to_string()),
            ]
        );
    }
}
