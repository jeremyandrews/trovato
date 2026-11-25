//! Scheduled publishing plugin for Trovato.
//!
//! Allows scheduling items for future publish/unpublish via
//! field_publish_on and field_unpublish_on JSONB fields.
//!
//! Implements `tap_cron` to process scheduled publish/unpublish
//! operations each cron cycle: a raw read finds what is due, and each change
//! is an item API write.

use trovato_sdk::host;
use trovato_sdk::prelude::*;

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        "schedule publishing",
        "Schedule content publishing",
    )]
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/content/scheduled", "Scheduled")
            .permission("schedule publishing")
            .parent("/admin/content"),
    ]
}

/// Most items published, and most unpublished, per cron run; the rest are
/// due again next run.
const BATCH: i64 = 200;

/// Items due to be published: unpublished, with a `field_publish_on` at or
/// before `now`.
const DUE_TO_PUBLISH: &str = "SELECT id::text AS id FROM item \
     WHERE status = 0 \
     AND fields->>'field_publish_on' IS NOT NULL \
     AND (fields->>'field_publish_on') ~ '^[0-9]+$' \
     AND (fields->>'field_publish_on')::bigint <= $1 \
     ORDER BY id LIMIT $2";

/// Items due to be unpublished: published, with a `field_unpublish_on` at or
/// before `now`.
const DUE_TO_UNPUBLISH: &str = "SELECT id::text AS id FROM item \
     WHERE status = 1 \
     AND fields->>'field_unpublish_on' IS NOT NULL \
     AND (fields->>'field_unpublish_on') ~ '^[0-9]+$' \
     AND (fields->>'field_unpublish_on')::bigint <= $1 \
     ORDER BY id LIMIT $2";

/// The ids a due query returns.
fn due(sql: &str, now: i64) -> Vec<String> {
    let Ok(json) = host::query_raw(sql, &[serde_json::json!(now), serde_json::json!(BATCH)]) else {
        return Vec::new();
    };
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap_or_default();
    rows.iter()
        .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(str::to_string))
        .collect()
}

/// Set each item's status through the item API: an item service update, so
/// the write taps fire, a revision is recorded, the item cache is refreshed
/// and the item is queued for the search index, as for an editor's save.
/// Returns how many were saved.
fn set_status(ids: &[String], status: i64, log: &str) -> u64 {
    let mut saved = 0;
    for id in ids {
        match host::save_item(&serde_json::json!({"id": id, "status": status, "log": log})) {
            Ok(Some(_)) => saved += 1,
            Ok(None) => {}
            Err(code) => host::log(
                "warn",
                "trovato_scheduled_publishing",
                &format!("could not set item {id} to status {status}: host error {code}"),
            ),
        }
    }
    saved
}

/// Process scheduled publish/unpublish operations.
///
/// Called each cron cycle. Publishes items whose `field_publish_on` is at or
/// before now and unpublishes items whose `field_unpublish_on` is, through
/// the item API rather than raw SQL, so a scheduled change is a real item
/// write: in particular the search index picks it up within one cron pass.
#[plugin_tap]
pub fn tap_cron(input: CronInput) -> serde_json::Value {
    let now = input.timestamp;
    let published = set_status(&due(DUE_TO_PUBLISH, now), 1, "Scheduled publish");
    let unpublished = set_status(&due(DUE_TO_UNPUBLISH, now), 0, "Scheduled unpublish");
    serde_json::json!({"published": published, "unpublished": unpublished})
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn perm_returns_one_permission() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 1);
    }

    #[test]
    fn menu_returns_one_route() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].path, "/admin/content/scheduled");
    }

    #[test]
    fn tap_cron_returns_counts() {
        let input = CronInput {
            timestamp: 1_700_000_000,
        };
        let result = __inner_tap_cron(input);
        // Stub host functions return 0 for both
        assert_eq!(result["published"], 0);
        assert_eq!(result["unpublished"], 0);
    }
}
