//! Scheduled publishing plugin for Trovato.
//!
//! Allows scheduling items for future publish/unpublish via
//! field_publish_on and field_unpublish_on JSONB fields.
//!
//! An item saved with a future `field_publish_on` or `field_unpublish_on`
//! gets a precise timer for that second (B278): `publish:<id>` or
//! `unpublish:<id>`, replaced when the date changes and cancelled when it is
//! cleared or the item is deleted. When it fires, `tap_timer` claims the item
//! by removing the date it applies, with a conditional statement, and an item
//! API save changes its status, so the item goes live at the second.
//! If that save fails, the date is put back and the failure reported, so the
//! timer tries again and, if it never succeeds, the cron sweep still sees it.
//!
//! `tap_cron` stays as the backstop: each run it finds whatever is due and
//! was not handled by a timer (an item scheduled before timers existed, a
//! date written straight to the table, a timer that failed), and applies it
//! the same way.

use trovato_sdk::host;
use trovato_sdk::prelude::*;
use trovato_sdk::types::{TimerInput, TimerRequest};

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

/// Put a claimed publish date back, when the save that should have applied it
/// failed, so the next attempt (the timer's retry, or the cron sweep) finds it.
/// Only where the date is still absent: an editor's new date wins.
const RESTORE_PUBLISH: &str = "UPDATE item SET fields = fields || \
     jsonb_build_object('field_publish_on', $2::bigint) \
     WHERE id = $1::uuid AND NOT (fields ? 'field_publish_on')";

/// [`RESTORE_PUBLISH`] for an unpublish date.
const RESTORE_UNPUBLISH: &str = "UPDATE item SET fields = fields || \
     jsonb_build_object('field_unpublish_on', $2::bigint) \
     WHERE id = $1::uuid AND NOT (fields ? 'field_unpublish_on')";

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

/// What became of one item's scheduled change.
enum Applied {
    /// Claimed and saved.
    Saved,
    /// No longer due (changed since it was found), or gone by the save.
    NotDue,
    /// The claim failed; the date is untouched.
    ClaimFailed(i32),
    /// The date was claimed (removed) and the save failed.
    SaveFailed(i32),
}

/// Claim item `id`'s date with `claim`, then set its status through the item
/// API: an item service update, so the write taps fire, a revision is
/// recorded, the item cache is refreshed and the item is queued for the search
/// index, as for an editor's save.
fn apply_one(id: &str, claim: &str, now: i64, status: i64, log: &str) -> Applied {
    match host::execute_raw(claim, &[serde_json::json!(id), serde_json::json!(now)]) {
        Ok(1) => {}
        Ok(_) => return Applied::NotDue,
        Err(code) => return Applied::ClaimFailed(code),
    }
    match host::save_item(&serde_json::json!({"id": id, "status": status, "log": log})) {
        Ok(Some(_)) => Applied::Saved,
        Ok(None) => Applied::NotDue,
        Err(code) => Applied::SaveFailed(code),
    }
}

/// [`apply_one`] for each due item, as the cron sweep runs it. Returns how
/// many were saved.
fn apply(ids: &[String], claim: &str, now: i64, status: i64, log: &str) -> u64 {
    let mut saved = 0;
    for id in ids {
        match apply_one(id, claim, now, status, log) {
            Applied::Saved => saved += 1,
            Applied::NotDue => {}
            Applied::ClaimFailed(code) => host::log(
                "warn",
                "trovato_scheduled_publishing",
                &format!("could not claim the schedule of item {id}: host error {code}"),
            ),
            Applied::SaveFailed(code) => host::log(
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

/// The two scheduled changes: the date field each reads, the status the item
/// must have for it to apply, the status it sets, the claim, the revision log
/// and the timer name prefix.
struct Change {
    field: &'static str,
    from_status: i64,
    to_status: i64,
    claim: &'static str,
    restore: &'static str,
    log: &'static str,
    prefix: &'static str,
}

const PUBLISH: Change = Change {
    field: "field_publish_on",
    from_status: 0,
    to_status: 1,
    claim: CLAIM_PUBLISH,
    restore: RESTORE_PUBLISH,
    log: "Scheduled publish",
    prefix: "publish",
};

const UNPUBLISH: Change = Change {
    field: "field_unpublish_on",
    from_status: 1,
    to_status: 0,
    claim: CLAIM_UNPUBLISH,
    restore: RESTORE_UNPUBLISH,
    log: "Scheduled unpublish",
    prefix: "unpublish",
};

/// A date field's value: Unix seconds, stored as a number or as digits.
fn date(fields: Option<&serde_json::Value>, field: &str) -> Option<i64> {
    match fields?.get(field)? {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
            s.parse().ok()
        }
        _ => None,
    }
}

/// The timer name for `change` on item `id`.
fn timer_name(change: &Change, id: &str) -> String {
    format!("{}:{id}", change.prefix)
}

/// Keep an item's timers in step with its dates: a date its status can use
/// gets a timer for that second (setting it again replaces it, so a changed
/// date moves it), and no such date cancels it.
fn sync_timers(item: &serde_json::Value) {
    let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
        return;
    };
    let status = item.get("status").and_then(|v| v.as_i64()).unwrap_or(0);
    for change in [&PUBLISH, &UNPUBLISH] {
        let name = timer_name(change, id);
        match date(item.get("fields"), change.field).filter(|_| status == change.from_status) {
            Some(at) => {
                let request = TimerRequest {
                    name: name.clone(),
                    run_at: at,
                    repeat_secs: None,
                    payload: serde_json::json!({ "item": id, "at": at }),
                };
                if let Err(code) = host::timer_set(&request) {
                    // The cron sweep still applies it, at the next run.
                    host::log(
                        "warn",
                        "trovato_scheduled_publishing",
                        &format!(
                            "could not set timer {name}: host error {code}; cron will apply it"
                        ),
                    );
                }
            }
            None => {
                let _ = host::timer_cancel(&name);
            }
        }
    }
}

/// An item was created: time its dates.
#[plugin_tap]
pub fn tap_item_insert(item: serde_json::Value) -> serde_json::Value {
    sync_timers(&item);
    serde_json::json!({})
}

/// An item was saved: move, set or cancel its timers to match its dates.
#[plugin_tap]
pub fn tap_item_update(item: serde_json::Value) -> serde_json::Value {
    sync_timers(&item);
    serde_json::json!({})
}

/// An item is being deleted: cancel its timers.
#[plugin_tap]
pub fn tap_item_delete(item: serde_json::Value) -> serde_json::Value {
    if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
        for change in [&PUBLISH, &UNPUBLISH] {
            let _ = host::timer_cancel(&timer_name(change, id));
        }
    }
    serde_json::json!({})
}

/// A date's timer fired: apply it to that item, through the same claim and
/// item API save as the cron sweep, at the time the timer fired.
#[plugin_tap]
pub fn tap_timer(input: TimerInput) -> serde_json::Value {
    let Some((prefix, id)) = input.name.split_once(':') else {
        return serde_json::json!({ "skipped": input.name });
    };
    let change = match prefix {
        "publish" => &PUBLISH,
        "unpublish" => &UNPUBLISH,
        _ => return serde_json::json!({ "skipped": input.name }),
    };
    let done =
        |saved: u64| serde_json::json!({ "item": id, "status": change.to_status, "saved": saved });
    match apply_one(
        id,
        change.claim,
        input.fired_at,
        change.to_status,
        change.log,
    ) {
        Applied::Saved => done(1),
        Applied::NotDue => done(0),
        // A failure is reported as one, so the timer is tried again (and,
        // after its last attempt, shown as failed); the date stays on the item
        // for the retry and for the cron sweep.
        Applied::ClaimFailed(code) => serde_json::json!({
            "error": format!("could not claim the schedule of item {id}: host error {code}")
        }),
        Applied::SaveFailed(code) => {
            let at = input
                .payload
                .get("at")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(input.run_at);
            let put_back = match host::execute_raw(
                change.restore,
                &[serde_json::json!(id), serde_json::json!(at)],
            ) {
                Ok(_) => "its date is kept".to_string(),
                Err(restore) => format!("its date could not be put back: host error {restore}"),
            };
            serde_json::json!({
                "error": format!(
                    "could not set item {id} to status {}: host error {code}; {put_back}",
                    change.to_status
                )
            })
        }
    }
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
