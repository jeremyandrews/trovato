//! Scheduled publishing plugin for Trovato.
//!
//! Allows scheduling items for future publish/unpublish via
//! field_publish_on and field_unpublish_on JSONB fields.
//!
//! Implements `tap_cron` to process scheduled publish/unpublish
//! operations each cron cycle: a raw read finds what is due, a conditional
//! statement claims each item by removing the date it applies, and an item API
//! save changes its status.

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
     AND (fields->>'field_publish_on') ~ '^[0-9]+$' \
     AND (fields->>'field_publish_on')::bigint <= $1 \
     ORDER BY id LIMIT $2";

/// Items due to be unpublished: published, with a `field_unpublish_on` at or
/// before `now`.
const DUE_TO_UNPUBLISH: &str = "SELECT id::text AS id FROM item \
     WHERE status = 1 \
     AND (fields->>'field_unpublish_on') ~ '^[0-9]+$' \
     AND (fields->>'field_unpublish_on')::bigint <= $1 \
     ORDER BY id LIMIT $2";

/// Claim one item's publish date if the item is still due: remove the date,
/// in one statement that checks it again. An editor's change between the due
/// read and the claim (a postponed or removed date, a manual publish) is
/// therefore respected. A date is applied once, not at every run while it is
/// past; otherwise an item with both dates past would be published and
/// unpublished at every run, and keep its place at the head of the batch.
const CLAIM_PUBLISH: &str = "UPDATE item SET fields = fields - 'field_publish_on' \
     WHERE id = $1::uuid AND status = 0 \
     AND (fields->>'field_publish_on') ~ '^[0-9]+$' \
     AND (fields->>'field_publish_on')::bigint <= $2";

/// [`CLAIM_PUBLISH`] for an unpublish date.
const CLAIM_UNPUBLISH: &str = "UPDATE item SET fields = fields - 'field_unpublish_on' \
     WHERE id = $1::uuid AND status = 1 \
     AND (fields->>'field_unpublish_on') ~ '^[0-9]+$' \
     AND (fields->>'field_unpublish_on')::bigint <= $2";

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

/// Claim each due item with `claim`, then set its status through the item
/// API: an item service update, so the write taps fire, a revision is
/// recorded, the item cache is refreshed and the item is queued for the search
/// index, as for an editor's save. Returns how many were saved.
fn apply(ids: &[String], claim: &str, now: i64, status: i64, log: &str) -> u64 {
    let mut saved = 0;
    for id in ids {
        match host::execute_raw(claim, &[serde_json::json!(id), serde_json::json!(now)]) {
            Ok(1) => {}
            // No longer due: changed since the due read.
            Ok(_) => continue,
            Err(code) => {
                host::log(
                    "warn",
                    "trovato_scheduled_publishing",
                    &format!("could not claim the schedule of item {id}: host error {code}"),
                );
                continue;
            }
        }
        match host::save_item(&serde_json::json!({"id": id, "status": status, "log": log})) {
            Ok(Some(_)) => saved += 1,
            Ok(None) => {}
            Err(code) => host::log(
                "warn",
                "trovato_scheduled_publishing",
                &format!(
                    "could not set item {id} to status {status}, and its date is removed: \
                     host error {code}"
                ),
            ),
        }
    }
    saved
}

/// Process scheduled publish/unpublish operations.
///
/// Called each cron cycle. Publishes items whose `field_publish_on` is at or
/// before now and unpublishes items whose `field_unpublish_on` is, removing
/// the date it applies, through the item API rather than raw SQL, so a
/// scheduled change is a real item write.
#[plugin_tap]
pub fn tap_cron(input: CronInput) -> serde_json::Value {
    let now = input.timestamp;
    let published = apply(
        &due(DUE_TO_PUBLISH, now),
        CLAIM_PUBLISH,
        now,
        1,
        "Scheduled publish",
    );
    let unpublished = apply(
        &due(DUE_TO_UNPUBLISH, now),
        CLAIM_UNPUBLISH,
        now,
        0,
        "Scheduled unpublish",
    );
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
