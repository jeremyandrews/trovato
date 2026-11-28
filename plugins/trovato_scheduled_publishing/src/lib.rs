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
//!
//! B326 adds the "Publishing schedule" group to the item form itself, for a
//! holder of `schedule publishing`: see the module doc nearer the bottom of
//! this file, by `tap_form_alter`, for how the two dates get from there into
//! `field_publish_on`/`field_unpublish_on` above.

use trovato_sdk::host;
use trovato_sdk::prelude::*;
use trovato_sdk::render::escape_html;
use trovato_sdk::types::{TimerInput, TimerRequest};

/// Held to see and set the item form's schedule group (B326), and to see
/// and change the admin listing below.
const PERM: &str = "schedule publishing";

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        PERM,
        "Schedule content publishing",
    )]
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/content/scheduled", "Scheduled")
            .permission(PERM)
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

/// A date value: Unix seconds, stored (or posted, on the JSON item API, B326)
/// as a number or as digits.
fn as_unix_seconds(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
            s.parse().ok()
        }
        _ => None,
    }
}

/// A date field's value out of an item's `fields` object (the taps' own
/// `serde_json::Value` item shape).
fn date(fields: Option<&serde_json::Value>, field: &str) -> Option<i64> {
    as_unix_seconds(fields?.get(field)?)
}

/// [`date`], out of a `fields` map ([`read_item`]'s shape), rather than a
/// `serde_json::Value` object.
fn item_date(fields: &std::collections::HashMap<String, serde_json::Value>, field: &str) -> Option<i64> {
    as_unix_seconds(fields.get(field)?)
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

// =============================================================================
// Scheduling on the item form (B326)
// =============================================================================
//
// Nothing wrote `field_publish_on`/`field_unpublish_on` before this: no
// content type declared them, and the item form had no input for them (B278's
// own test had to add an integer field by hand in the admin and type a Unix
// timestamp). This adds a "Publishing schedule" group to the normal item add
// and edit form, for a holder of `schedule publishing`, with two
// `datetime-local` inputs read in the viewer's own time zone and converted to
// and from UTC seconds through Postgres (`AT TIME ZONE`), the same mechanism
// B278's admin timers report already uses from the kernel side
// (`routes/admin_timers.rs`) — no time zone library in this plugin.
//
// `content::submission::build_item_fields` (the item form's save path) only
// ever writes a content type's *declared* fields, so an input this plugin adds
// through `tap_form_alter` under a name no type declares never reaches
// `item.fields` that way. Taking the values from `tap_form_submit` instead
// (posted, after the save, filtered to what the form rendered rather than to
// the type's schema) and writing them through `host::save_item`, with no
// kernel change, closes that gap — but only with the item's *complete*
// current `fields`, read once just before the write and changed in just the
// two schedule keys: `host::save_item` with a *partial* `fields` object would
// silently wipe every other field (`gate_field_writes` starts from the
// *submitted* object, not the stored one). A single-key raw SQL merge (this
// plugin's own cron path uses one, for its claim-and-restore dance) would
// dodge that trap too, but would not invalidate the item cache the way a real
// `ItemService` write does, so a raw-written date would not reliably show
// back on the edit form, or reach `tap_item_update`'s `sync_timers`, until
// some unrelated save happened to refresh it — `tap_form_submit`, below, has
// the detail.
//
// The one kernel seam this needed: `tap_form_validate`'s dispatched input
// gained `item_id` (it carried only `form_id`/`values`), and a new
// `FormService::validate_fields` dispatches that same tap for the JSON item
// API, with a synthetic `json_item_{type}` form id — so the one
// `tap_form_validate` below guards both paths, as the form is not the only
// guard a client can reach.

/// The group's two inputs: a `datetime-local` value in the viewer's own time
/// zone. Distinct from the stored keys (`field_publish_on`/`field_unpublish_on`,
/// Unix seconds, below) so a content type that happens to declare a field by
/// either stored name is never shadowed by this group.
const IN_PUBLISH: &str = "schedule_publish_on";
const IN_UNPUBLISH: &str = "schedule_unpublish_on";

/// Whether `form_id` is an item add or edit form, for any content type:
/// `Some(true)` edit, `Some(false)` add, `None` anything else (including the
/// JSON item API's synthetic `json_item_{type}`, checked separately where it
/// applies).
fn item_form_kind(form_id: &str) -> Option<bool> {
    if form_id.starts_with("item_edit_") {
        Some(true)
    } else if form_id.starts_with("item_add_") {
        Some(false)
    } else {
        None
    }
}

/// A loose but sufficient UUID shape check: the hyphens at the fixed
/// positions, hex digits elsewhere. Good enough to tell an id apart from
/// `add` or a type's machine name in [`item_id_from_action`]; a real check
/// happens in SQL regardless.
fn is_uuid(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 36
        && [8, 13, 18, 23].iter().all(|&i| bytes[i] == b'-')
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| [8, 13, 18, 23].contains(&i) || b.is_ascii_hexdigit())
}

/// The item id in an item form's action url (`/item/{id}/edit` or
/// `/admin/content/{id}/edit`) — the one place the built `Form` carries it,
/// since `tap_form_alter`'s input has no `item_id` of its own (unlike
/// `tap_form_validate` and `tap_form_submit`, below, which do). `None` for an
/// add form, whose action names no id.
fn item_id_from_action(action: &str) -> Option<String> {
    let segments: Vec<&str> = action.trim_matches('/').split('/').collect();
    let (last, rest) = segments.split_last()?;
    if *last != "edit" {
        return None;
    }
    let candidate = *rest.last()?;
    is_uuid(candidate).then(|| candidate.to_string())
}

/// One item's `fields` and `status`, read straight off the `item` table
/// (`host::query_raw`), not through `host::get_item`.
///
/// `host::get_item` decides the call as the acting user and answers a
/// denied **view** the same as a missing item — but an editor often holds
/// `edit` on their own unpublished draft without separately holding `view
/// own unpublished content`, a different permission axis, and every caller
/// here (`tap_form_alter`, `tap_form_validate`, `tap_form_submit`) only ever
/// reads an item the kernel's own route has *already* decided this request
/// may edit (the form would not have built, or the save would not have
/// happened, otherwise). Reading past `get_item`'s view gate is therefore not
/// a new read this user was not already allowed, and the alternative —
/// `get_item` wrongly answering "no such item" for the very item being
/// edited — would hide an already-stored schedule from the reopened form,
/// mis-score "already published" in validation, and silently drop a write in
/// `tap_form_submit`. `item` is not a protected table (`db_policy.rs`), so
/// `query_raw` carries it.
fn read_item(item_id: &str) -> Option<(std::collections::HashMap<String, serde_json::Value>, i64)> {
    let json = host::query_raw(
        "SELECT fields, status FROM item WHERE id = $1::uuid",
        &[serde_json::json!(item_id)],
    )
    .ok()?;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).ok()?;
    let row = rows.first()?;
    let fields = serde_json::from_value(row.get("fields")?.clone()).ok()?;
    let status = row.get("status")?.as_i64()?;
    Some((fields, status))
}

/// The signed-in user's time zone: `host::load_users` (`users` is a
/// protected table, off limits to `query_raw` — see
/// `crates/kernel/src/plugin/db_policy.rs`'s `PROTECTED_TABLES` — so this is
/// the one sanctioned way to read it), checked against Postgres, else UTC
/// when it is unset or empty or Postgres does not know it. No site-wide
/// default time zone exists in this codebase to fall back to first. The
/// Postgres check mirrors `routes/admin_timers.rs`'s `viewer_zone` (B278),
/// from inside a plugin rather than the kernel.
fn viewer_zone() -> String {
    let uid = host::current_user_id();
    if uid.is_empty() {
        return "UTC".to_string();
    }
    let zone = host::load_users(&[uid.as_str()])
        .ok()
        .and_then(|users| users.into_iter().next())
        .and_then(|u| u.timezone)
        .map(|z| z.trim().to_string())
        .filter(|z| !z.is_empty());
    let Some(zone) = zone else {
        return "UTC".to_string();
    };
    match host::query_raw("SELECT now() AT TIME ZONE $1", &[serde_json::json!(zone)]) {
        Ok(_) => zone,
        Err(_) => "UTC".to_string(),
    }
}

/// `secs` (Unix seconds) as a `datetime-local` value in `zone` — the shape
/// the input re-reads, `YYYY-MM-DDTHH:MM`. `None` on a host error: the SQL is
/// fixed and `secs` always ours, so this should not happen, but a form that
/// cannot prefill a value shows it empty rather than refusing to render.
fn utc_to_local(secs: i64, zone: &str) -> Option<String> {
    let json = host::query_raw(
        "SELECT to_char(to_timestamp($1) AT TIME ZONE $2, 'YYYY-MM-DD\"T\"HH24:MI') AS local",
        &[serde_json::json!(secs), serde_json::json!(zone)],
    )
    .ok()?;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).ok()?;
    rows.first()?.get("local")?.as_str().map(str::to_string)
}

/// The result of reading one `datetime-local` value typed in a time zone.
enum Converted {
    /// A real instant. The earlier of two when the typed time fell in a DST
    /// overlap (occurs twice, each a real moment — the earlier is as good an
    /// answer as the later, and a deterministic one).
    At(i64),
    /// The typed time does not exist in the zone: a DST gap, where clocks
    /// skip over it.
    Gap,
    /// Not a value Postgres can read as a local date and time at all.
    Unreadable,
}

/// Convert `local` (a `datetime-local` value, with or without seconds) typed
/// in `zone` to Unix seconds, detecting a DST gap or overlap.
///
/// Postgres's own `AT TIME ZONE` resolves an ambiguous or skipped local time
/// to one answer with no way to ask it for the other (always the *later* of
/// the two real instants for an overlap, confirmed against a live Postgres;
/// and for a gap, the offset that applies *after* the skip, which silently
/// shifts the result by the jump rather than refusing it) — so this asks
/// twice instead: with the UTC offset in force two hours before `local` and
/// two hours after, both safely outside any real-world transition window
/// (under two hours everywhere), applied to `local` by plain interval
/// arithmetic rather than by asking `AT TIME ZONE` again. Each candidate is
/// then checked by converting it back and seeing whether that reproduces
/// `local`: neither doing so is a gap (`local` names no real instant); both
/// doing so is an overlap (two different real instants read back to the same
/// wall clock), and the earlier of the two is taken.
fn local_to_utc(local: &str, zone: &str) -> Converted {
    const SQL: &str = "WITH probe AS (SELECT $1::timestamp AS wall, $2::text AS zone), \
         cand AS ( \
           SELECT wall, zone, \
             ((wall - interval '2 hours') AT TIME ZONE zone) + interval '2 hours' AS before_cand, \
             ((wall + interval '2 hours') AT TIME ZONE zone) - interval '2 hours' AS after_cand \
           FROM probe \
         ) \
         SELECT EXTRACT(EPOCH FROM before_cand)::bigint AS before_epoch, \
                EXTRACT(EPOCH FROM after_cand)::bigint AS after_epoch, \
                (before_cand AT TIME ZONE zone) = wall AS before_rt, \
                (after_cand AT TIME ZONE zone) = wall AS after_rt \
         FROM cand";
    let Ok(json) = host::query_raw(SQL, &[serde_json::json!(local), serde_json::json!(zone)])
    else {
        return Converted::Unreadable;
    };
    let Ok(rows) = serde_json::from_str::<Vec<serde_json::Value>>(&json) else {
        return Converted::Unreadable;
    };
    let Some(row) = rows.first() else {
        return Converted::Unreadable;
    };
    let before_rt = row
        .get("before_rt")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let after_rt = row
        .get("after_rt")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let before = row.get("before_epoch").and_then(serde_json::Value::as_i64);
    let after = row.get("after_epoch").and_then(serde_json::Value::as_i64);
    match (before_rt, after_rt, before, after) {
        (false, false, _, _) => Converted::Gap,
        (true, true, Some(b), Some(a)) => Converted::At(b.min(a)),
        (true, _, Some(b), _) => Converted::At(b),
        (_, true, _, Some(a)) => Converted::At(a),
        _ => Converted::Unreadable,
    }
}

/// The real time, in Unix seconds. WASI's realtime clock (the kernel's
/// plugin runtime grants it unconditionally, not gated by a capability; see
/// `crates/kernel/src/plugin/runtime.rs`), so this needs no host function of
/// its own.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// One schedule field's raw posted value, resolved.
enum Resolved {
    /// The form did not render this input (no permission), or the JSON body
    /// did not name this key: leave whatever is stored exactly as it is.
    Unset,
    /// Posted empty: remove the stored date, if any.
    Clear,
    /// A valid instant, in Unix seconds.
    At(i64),
    /// (Form path only.) The typed local time does not exist (a DST gap).
    Gap,
    /// Not a value this path's shape can read at all.
    Unreadable,
}

/// Resolve one schedule field's raw value. `zone` is `Some` on the form path
/// (`raw` is a `datetime-local` string, read in that time zone); `None` on
/// the JSON item API, whose value is already Unix seconds (a number or a
/// string of digits, [`as_unix_seconds`]), so no conversion applies and no
/// DST question arises.
fn resolve(raw: Option<&serde_json::Value>, zone: Option<&str>) -> Resolved {
    let Some(raw) = raw else {
        return Resolved::Unset;
    };
    match zone {
        Some(zone) => {
            let text = match raw {
                serde_json::Value::String(s) => s.trim().to_string(),
                serde_json::Value::Null => String::new(),
                _ => return Resolved::Unreadable,
            };
            if text.is_empty() {
                return Resolved::Clear;
            }
            match local_to_utc(&text, zone) {
                Converted::At(secs) => Resolved::At(secs),
                Converted::Gap => Resolved::Gap,
                Converted::Unreadable => Resolved::Unreadable,
            }
        }
        None => {
            let empty = matches!(raw, serde_json::Value::String(s) if s.trim().is_empty());
            if raw.is_null() || empty {
                return Resolved::Clear;
            }
            match as_unix_seconds(raw) {
                Some(secs) => Resolved::At(secs),
                None => Resolved::Unreadable,
            }
        }
    }
}

/// Which of the two schedule fields an error names.
#[derive(Clone, Copy)]
enum Which {
    Publish,
    Unpublish,
}

/// The refusals one submission's resolved publish and unpublish values earn,
/// against the item's current stored state. The one function both
/// `tap_form_validate` call sites run (the form, and the JSON item API's
/// synthetic form id), so a submission cannot reach a rule the other path
/// enforced.
///
/// - A past date is refused unless it is unchanged from what is stored
///   (resaving an item whose time passed a second ago must not fail).
/// - "Publish on" is refused on an item that is already published;
///   "Unpublish on" is refused on an item that is unpublished with no
///   "Publish on" pending (effective, after this submission, not only
///   stored) — either way nothing to schedule, and a plain early refusal
///   reads better than a timer that would silently never fire.
/// - "Unpublish on" must be after "Publish on" when both will end up set,
///   checked only when at least one of the two was actually touched by this
///   submission: a save that touches neither must not newly fail on a pair
///   that predates this check.
fn schedule_errors(
    publish: &Resolved,
    unpublish: &Resolved,
    current_status: i64,
    stored_publish: Option<i64>,
    stored_unpublish: Option<i64>,
    now: i64,
) -> Vec<(Which, String)> {
    let mut errors = Vec::new();
    for (which, resolved) in [(Which::Publish, publish), (Which::Unpublish, unpublish)] {
        match resolved {
            Resolved::Gap => errors.push((
                which,
                "That local time does not exist: clocks move forward past it for daylight \
                 saving. Choose a different time."
                    .to_string(),
            )),
            Resolved::Unreadable => errors.push((which, "Enter a date and time.".to_string())),
            _ => {}
        }
    }

    let effective_publish = match publish {
        Resolved::At(v) => Some(*v),
        Resolved::Clear => None,
        Resolved::Unset | Resolved::Gap | Resolved::Unreadable => stored_publish,
    };
    let effective_unpublish = match unpublish {
        Resolved::At(v) => Some(*v),
        Resolved::Clear => None,
        Resolved::Unset | Resolved::Gap | Resolved::Unreadable => stored_unpublish,
    };

    if let Resolved::At(v) = publish {
        if *v < now && stored_publish != Some(*v) {
            errors.push((
                Which::Publish,
                "Publish on must be a date and time in the future.".to_string(),
            ));
        }
        if current_status == 1 {
            errors.push((
                Which::Publish,
                "This item is already published; unpublish it or clear the date.".to_string(),
            ));
        }
    }
    if let Resolved::At(v) = unpublish {
        if *v < now && stored_unpublish != Some(*v) {
            errors.push((
                Which::Unpublish,
                "Unpublish on must be a date and time in the future.".to_string(),
            ));
        }
        if current_status == 0 && effective_publish.is_none() {
            errors.push((
                Which::Unpublish,
                "This item is already unpublished; publish it, schedule a publish date, or \
                 clear this date."
                    .to_string(),
            ));
        }
    }

    let touched = !matches!(publish, Resolved::Unset) || !matches!(unpublish, Resolved::Unset);
    if touched
        && let (Some(p), Some(u)) = (effective_publish, effective_unpublish)
        && u <= p
    {
        errors.push((
            Which::Unpublish,
            "Unpublish on must be after publish on.".to_string(),
        ));
    }
    errors
}

/// The group's two inputs and their hints, filled with `stored_publish` /
/// `stored_unpublish` (Unix seconds) shown in `zone`, plus one status line
/// each when a date is stored: "Scheduled to publish/unpublish on …".
fn schedule_fieldset(
    zone: &str,
    stored_publish: Option<i64>,
    stored_unpublish: Option<i64>,
    weight: i32,
) -> serde_json::Value {
    let zone_html = escape_html(zone);
    let mut children = serde_json::Map::new();

    let field = |name: &str, label: &str, default: Option<i64>, weight: i32| {
        serde_json::json!({
            "type": "input",
            "input_type": "datetime-local",
            "title": format!("{label} ({zone})"),
            "default_value": default.and_then(|s| utc_to_local(s, zone)).unwrap_or_default(),
            "attributes": {"aria-describedby": format!("{name}-hint"), "step": "60"},
            "weight": weight,
        })
    };
    let hint = |name: &str, weight: i32| {
        serde_json::json!({
            "type": "markup",
            "value": format!(
                "<p id=\"{name}-hint\" class=\"form-item__description\">Read and saved in your \
                 account's time zone, {zone_html}. Leave empty for none.</p>"
            ),
            "weight": weight,
        })
    };
    let status = |label: &str, secs: i64, weight: i32| {
        let local = utc_to_local(secs, zone).unwrap_or_default().replace('T', " ");
        serde_json::json!({
            "type": "markup",
            "value": format!(
                "<p class=\"schedule-status\">{label} {} {zone_html}</p>",
                escape_html(&local)
            ),
            "weight": weight,
        })
    };

    children.insert(
        IN_PUBLISH.to_string(),
        field(IN_PUBLISH, "Publish on", stored_publish, 0),
    );
    children.insert(format!("_{IN_PUBLISH}_hint"), hint(IN_PUBLISH, 1));
    if let Some(secs) = stored_publish {
        children.insert(
            "_schedule_publish_status".to_string(),
            status("Scheduled to publish on", secs, 2),
        );
    }
    children.insert(
        IN_UNPUBLISH.to_string(),
        field(IN_UNPUBLISH, "Unpublish on", stored_unpublish, 3),
    );
    children.insert(format!("_{IN_UNPUBLISH}_hint"), hint(IN_UNPUBLISH, 4));
    if let Some(secs) = stored_unpublish {
        children.insert(
            "_schedule_unpublish_status".to_string(),
            status("Scheduled to unpublish on", secs, 5),
        );
    }

    serde_json::json!({
        "type": "fieldset",
        "title": "Publishing schedule",
        "weight": weight,
        "attributes": {"id": "schedule-publishing"},
        "children": children,
    })
}

/// Add the "Publishing schedule" group to an item add or edit form, for a
/// holder of [`PERM`]; nothing otherwise. After the kernel's own "Publishing
/// options" group (weight `STATUS_WEIGHT + 5`) and the stage selector
/// (`STATUS_WEIGHT + 10`), before the revision log (`LOG_WEIGHT`) and Save
/// (`SUBMIT_WEIGHT`) — see `crates/kernel/src/content/form.rs`.
#[plugin_tap]
pub fn tap_form_alter(mut form: serde_json::Value) -> serde_json::Value {
    const WEIGHT: i32 = 920;
    let form_id = form
        .get("form_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if item_form_kind(&form_id).is_none() || !host::current_user_has_permission(PERM) {
        return form;
    }
    let action = form
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let existing_fields = item_id_from_action(&action)
        .and_then(|id| read_item(&id))
        .map(|(fields, _status)| fields);
    let stored_publish = existing_fields
        .as_ref()
        .and_then(|f| item_date(f, "field_publish_on"));
    let stored_unpublish = existing_fields
        .as_ref()
        .and_then(|f| item_date(f, "field_unpublish_on"));
    let zone = viewer_zone();
    if let Some(elements) = form.get_mut("elements").and_then(|v| v.as_object_mut()) {
        elements.insert(
            "schedule_publishing".to_string(),
            schedule_fieldset(&zone, stored_publish, stored_unpublish, WEIGHT),
        );
    }
    form
}

/// The field name an error names, on each path: the group's own input on the
/// form, the stored key itself on the JSON item API (which has no local-time
/// input to translate back to).
fn error_field(is_json: bool, which: Which) -> &'static str {
    match (is_json, which) {
        (true, Which::Publish) => "field_publish_on",
        (true, Which::Unpublish) => "field_unpublish_on",
        (false, Which::Publish) => IN_PUBLISH,
        (false, Which::Unpublish) => IN_UNPUBLISH,
    }
}

/// Check a submission of the item form's schedule group, or of the JSON item
/// API's `fields.field_publish_on` / `fields.field_unpublish_on` (the
/// synthetic `json_item_{type}` form id `FormService::validate_fields`
/// dispatches this same tap with, B326).
#[plugin_tap]
pub fn tap_form_validate(input: serde_json::Value) -> serde_json::Value {
    let form_id = input
        .get("form_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let is_json = form_id.starts_with("json_item_");
    if !is_json && item_form_kind(form_id).is_none() {
        return serde_json::json!({});
    }
    // On the form path only a holder of the permission sees the group at
    // all, so only they can post it; the kernel's own `rendered_fields` gate
    // already keeps anyone else's post of these names out of `values`
    // regardless (`posted_values`, `crates/kernel/src/form/service.rs`), and
    // this is one more line of the same rule rather than the rule itself.
    // The JSON item API has no such gate: a client that may write the item's
    // other fields may write these, the same as any other undeclared key.
    if !is_json && !host::current_user_has_permission(PERM) {
        return serde_json::json!({});
    }
    let (publish_key, unpublish_key) = if is_json {
        ("field_publish_on", "field_unpublish_on")
    } else {
        (IN_PUBLISH, IN_UNPUBLISH)
    };
    let values = input.get("values").cloned().unwrap_or_default();
    let raw_publish = values.get(publish_key).cloned();
    let raw_unpublish = values.get(unpublish_key).cloned();
    if raw_publish.is_none() && raw_unpublish.is_none() {
        // Neither key was posted: nothing to check, and no need for the
        // item read below on what is, for most saves of most content types,
        // every single one.
        return serde_json::json!({});
    }

    let item_id = input.get("item_id").and_then(|v| v.as_str());
    let existing = item_id.and_then(read_item);
    let current_status = existing.as_ref().map_or(0, |(_, status)| *status);
    let stored_publish = existing
        .as_ref()
        .and_then(|(fields, _)| item_date(fields, "field_publish_on"));
    let stored_unpublish = existing
        .as_ref()
        .and_then(|(fields, _)| item_date(fields, "field_unpublish_on"));

    let zone = (!is_json).then(viewer_zone);
    let publish = resolve(raw_publish.as_ref(), zone.as_deref());
    let unpublish = resolve(raw_unpublish.as_ref(), zone.as_deref());

    let errors = schedule_errors(
        &publish,
        &unpublish,
        current_status,
        stored_publish,
        stored_unpublish,
        now(),
    );
    if errors.is_empty() {
        return serde_json::json!({});
    }
    let out: Vec<serde_json::Value> = errors
        .into_iter()
        .map(|(which, message)| {
            serde_json::json!({"field": error_field(is_json, which), "message": message})
        })
        .collect();
    serde_json::json!({"errors": out})
}

/// Apply one resolved schedule value to a clone of the item's current
/// `fields`: set it, clear it, or (`Unset`/`Gap`/`Unreadable`) leave it
/// exactly as it is.
fn apply_resolved(
    fields: &mut std::collections::HashMap<String, serde_json::Value>,
    key: &'static str,
    resolved: &Resolved,
) {
    match resolved {
        Resolved::At(secs) => {
            fields.insert(key.to_string(), serde_json::json!(secs));
        }
        Resolved::Clear => {
            fields.remove(key);
        }
        Resolved::Unset | Resolved::Gap | Resolved::Unreadable => {}
    }
}

/// A save of the item form's schedule group: write the resolved values (the
/// JSON item API needs no help — its own `field_publish_on`/`field_unpublish_on`
/// are ordinary, if undeclared, keys in the `fields` object it already sends
/// whole, so they are stored like any other field with no plugin
/// involvement).
///
/// `tap_item_insert`/`tap_item_update` already ran earlier in this same save,
/// against the item as it was *before* these dates existed on it (the form
/// path never gets them into `item.fields`, see the module doc) — so without
/// this, an item scheduled through the form would get no timer until some
/// later, unrelated save.
///
/// This writes through `host::save_item` with the item's *complete* current
/// `fields` ([`read_item`], not `host::get_item` — see its doc for why; read
/// once, before any write, so nothing this call itself did can make that
/// read stale) with only the two schedule keys changed —
/// never a partial object (the module doc explains why) — rather than the
/// single-key raw SQL merge this plugin's own cron path uses for its
/// claim-and-restore dance. A raw write does not invalidate the item cache
/// (`ItemService::load`'s in-memory cache, `content/item_service.rs`), so a
/// raw-written date would not reliably show back on the edit form or reach
/// `tap_item_update`'s `sync_timers` on a later read until some unrelated
/// save happened to refresh it. `save_item` is a real `ItemService` write: it
/// invalidates that cache and fires `tap_item_update` itself, which is what
/// actually sets or cancels the timer — no second call to `sync_timers`
/// needed here.
#[plugin_tap]
pub fn tap_form_submit(input: serde_json::Value) -> serde_json::Value {
    let form_id = input
        .get("form_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if item_form_kind(form_id).is_none() || !host::current_user_has_permission(PERM) {
        return serde_json::json!({});
    }
    let Some(item_id) = input.get("item_id").and_then(|v| v.as_str()) else {
        return serde_json::json!({});
    };
    let values = input.get("values").cloned().unwrap_or_default();
    let raw_publish = values.get(IN_PUBLISH).cloned();
    let raw_unpublish = values.get(IN_UNPUBLISH).cloned();
    if raw_publish.is_none() && raw_unpublish.is_none() {
        return serde_json::json!({});
    }
    let zone = viewer_zone();
    let publish = resolve(raw_publish.as_ref(), Some(&zone));
    let unpublish = resolve(raw_unpublish.as_ref(), Some(&zone));

    let Some((mut fields, _status)) = read_item(item_id) else {
        return serde_json::json!({});
    };
    apply_resolved(&mut fields, "field_publish_on", &publish);
    apply_resolved(&mut fields, "field_unpublish_on", &unpublish);

    if let Err(code) = host::save_item(&serde_json::json!({"id": item_id, "fields": fields})) {
        host::log(
            "error",
            "trovato_scheduled_publishing",
            &format!("could not write item {item_id}'s schedule: host error {code}"),
        );
    }
    serde_json::json!({})
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

    // =========================================================================
    // B326: scheduling on the item form
    // =========================================================================

    #[test]
    fn item_form_kind_recognizes_add_and_edit_of_any_type() {
        assert_eq!(item_form_kind("item_add_blog"), Some(false));
        assert_eq!(item_form_kind("item_edit_page"), Some(true));
        assert_eq!(item_form_kind("item_edit_blog_post"), Some(true));
        assert_eq!(item_form_kind("user_profile_form"), None);
        assert_eq!(item_form_kind("json_item_blog"), None);
    }

    #[test]
    fn is_uuid_accepts_only_the_right_shape() {
        assert!(is_uuid("11111111-1111-1111-1111-111111111111"));
        assert!(!is_uuid("add"));
        assert!(!is_uuid("blog"));
        assert!(!is_uuid("11111111-1111-1111-1111-11111111111")); // one short
        assert!(!is_uuid("1111111181111-1111-1111-111111111111")); // hyphens moved
    }

    #[test]
    fn item_id_from_action_reads_the_edit_forms_and_nothing_else() {
        assert_eq!(
            item_id_from_action("/item/11111111-1111-1111-1111-111111111111/edit"),
            Some("11111111-1111-1111-1111-111111111111".to_string())
        );
        assert_eq!(
            item_id_from_action(
                "/admin/content/11111111-1111-1111-1111-111111111111/edit"
            ),
            Some("11111111-1111-1111-1111-111111111111".to_string())
        );
        assert_eq!(item_id_from_action("/item/add/blog"), None);
        assert_eq!(item_id_from_action("/admin/content/add/blog"), None);
    }

    #[test]
    fn as_unix_seconds_reads_a_number_or_digits_and_nothing_else() {
        assert_eq!(as_unix_seconds(&serde_json::json!(42)), Some(42));
        assert_eq!(as_unix_seconds(&serde_json::json!("42")), Some(42));
        assert_eq!(as_unix_seconds(&serde_json::json!("")), None);
        assert_eq!(as_unix_seconds(&serde_json::json!("soon")), None);
        assert_eq!(as_unix_seconds(&serde_json::json!(null)), None);
    }

    /// A plain future date, past both current effective publish/unpublish
    /// dates, earns no error: the common case must stay quiet.
    #[test]
    fn a_plain_future_date_is_accepted() {
        let errors = schedule_errors(
            &Resolved::At(2_000_000_000),
            &Resolved::Unset,
            0,
            None,
            None,
            1_700_000_000,
        );
        assert!(errors.is_empty(), "{:?}", error_messages(&errors));
    }

    #[test]
    fn a_past_date_is_refused() {
        let errors = schedule_errors(
            &Resolved::At(1_000),
            &Resolved::Unset,
            0,
            None,
            None,
            1_700_000_000,
        );
        assert!(matches!(errors[0].0, Which::Publish));
        assert!(errors[0].1.contains("future"));
    }

    /// Resaving an item whose scheduled time passed a second ago must not
    /// fail: the exemption is "unchanged from what is stored", not "in the
    /// future".
    #[test]
    fn a_past_date_unchanged_from_what_is_stored_is_accepted() {
        let errors = schedule_errors(
            &Resolved::At(1_699_999_999),
            &Resolved::Unset,
            0,
            Some(1_699_999_999),
            None,
            1_700_000_000,
        );
        assert!(errors.is_empty(), "{:?}", error_messages(&errors));
    }

    #[test]
    fn unpublish_before_publish_is_refused() {
        let errors = schedule_errors(
            &Resolved::At(2_000_000_000),
            &Resolved::At(1_900_000_000),
            0,
            None,
            None,
            1_700_000_000,
        );
        assert!(
            errors
                .iter()
                .any(|(w, m)| matches!(w, Which::Unpublish) && m.contains("after")),
            "{:?}",
            error_messages(&errors)
        );
    }

    #[test]
    fn unpublish_equal_to_publish_is_refused() {
        let errors = schedule_errors(
            &Resolved::At(2_000_000_000),
            &Resolved::At(2_000_000_000),
            0,
            None,
            None,
            1_700_000_000,
        );
        assert!(
            errors
                .iter()
                .any(|(w, m)| matches!(w, Which::Unpublish) && m.contains("after")),
            "{:?}",
            error_messages(&errors)
        );
    }

    /// A resave that touches neither field must not newly fail on a
    /// pre-existing bad order: only a submission that changes one of the
    /// two dates is checked.
    #[test]
    fn an_untouched_pair_is_not_reordered_by_a_resave() {
        let errors = schedule_errors(
            &Resolved::Unset,
            &Resolved::Unset,
            0,
            Some(2_000_000_000),
            Some(1_900_000_000),
            1_700_000_000,
        );
        assert!(errors.is_empty(), "{:?}", error_messages(&errors));
    }

    #[test]
    fn publish_on_is_refused_on_an_already_published_item() {
        let errors = schedule_errors(
            &Resolved::At(2_000_000_000),
            &Resolved::Unset,
            1,
            None,
            None,
            1_700_000_000,
        );
        assert!(
            errors
                .iter()
                .any(|(w, m)| matches!(w, Which::Publish) && m.contains("already published")),
            "{:?}",
            error_messages(&errors)
        );
    }

    #[test]
    fn unpublish_on_is_refused_on_an_unpublished_item_with_no_pending_publish() {
        let errors = schedule_errors(
            &Resolved::Unset,
            &Resolved::At(2_000_000_000),
            0,
            None,
            None,
            1_700_000_000,
        );
        assert!(
            errors
                .iter()
                .any(|(w, m)| matches!(w, Which::Unpublish) && m.contains("already unpublished")),
            "{:?}",
            error_messages(&errors)
        );
    }

    /// The same unpublish date is fine when a publish is pending (stored or
    /// just set): "publish later, then take down even later."
    #[test]
    fn unpublish_on_is_accepted_on_an_unpublished_item_with_a_pending_publish() {
        let errors = schedule_errors(
            &Resolved::Unset,
            &Resolved::At(2_000_000_000),
            0,
            Some(1_900_000_000),
            None,
            1_700_000_000,
        );
        assert!(errors.is_empty(), "{:?}", error_messages(&errors));
    }

    #[test]
    fn a_gap_and_an_unreadable_value_are_each_refused_with_their_own_message() {
        let errors = schedule_errors(&Resolved::Gap, &Resolved::Unset, 0, None, None, 0);
        assert!(errors[0].1.contains("does not exist"));

        let errors = schedule_errors(&Resolved::Unreadable, &Resolved::Unset, 0, None, None, 0);
        assert_eq!(errors[0].1, "Enter a date and time.");
    }

    #[test]
    fn error_field_names_differ_by_path() {
        assert_eq!(error_field(false, Which::Publish), IN_PUBLISH);
        assert_eq!(error_field(false, Which::Unpublish), IN_UNPUBLISH);
        assert_eq!(error_field(true, Which::Publish), "field_publish_on");
        assert_eq!(error_field(true, Which::Unpublish), "field_unpublish_on");
    }

    fn error_messages(errors: &[(Which, String)]) -> Vec<&str> {
        errors.iter().map(|(_, m)| m.as_str()).collect()
    }

    #[test]
    fn resolve_treats_an_absent_key_as_unset_and_an_empty_one_as_clear() {
        assert!(matches!(resolve(None, Some("UTC")), Resolved::Unset));
        assert!(matches!(
            resolve(Some(&serde_json::json!("")), Some("UTC")),
            Resolved::Clear
        ));
        assert!(matches!(
            resolve(Some(&serde_json::Value::Null), None),
            Resolved::Clear
        ));
    }

    #[test]
    fn resolve_reads_unix_seconds_directly_on_the_json_path() {
        assert!(matches!(
            resolve(Some(&serde_json::json!(1_700_000_000)), None),
            Resolved::At(1_700_000_000)
        ));
        assert!(matches!(
            resolve(Some(&serde_json::json!("1700000000")), None),
            Resolved::At(1_700_000_000)
        ));
        assert!(matches!(
            resolve(Some(&serde_json::json!("soon")), None),
            Resolved::Unreadable
        ));
    }

    /// Permission and form-shape checks that do not need the database: a
    /// user without the permission sees no alteration (the stub always
    /// grants it, so this instead proves the function skips any form the
    /// permission check would have gated past — every form other than an
    /// item add/edit) and `tap_form_submit`/`tap_form_validate` ignore a
    /// form that is neither.
    #[test]
    fn the_new_taps_ignore_every_form_that_is_not_theirs() {
        let unrelated = serde_json::json!({
            "form_id": "user_login",
            "action": "/user/login",
            "elements": {"name": {"type": "textfield"}},
        });
        assert_eq!(
            __inner_tap_form_alter(unrelated.clone())["elements"],
            unrelated["elements"]
        );
        assert_eq!(
            __inner_tap_form_validate(serde_json::json!({
                "form_id": "user_login", "values": {}, "item_id": null,
            })),
            serde_json::json!({})
        );
        assert_eq!(
            __inner_tap_form_submit(serde_json::json!({
                "form_id": "user_login", "values": {}, "item_id": null,
            })),
            serde_json::json!({})
        );
    }

    #[test]
    fn alter_adds_the_schedule_group_to_an_item_add_form() {
        let form = serde_json::json!({
            "form_id": "item_add_blog",
            "action": "/item/add/blog",
            "elements": {},
        });
        let altered = __inner_tap_form_alter(form);
        let group = &altered["elements"]["schedule_publishing"];
        assert_eq!(group["type"], "fieldset");
        assert!(group["children"][IN_PUBLISH].is_object());
        assert!(group["children"][IN_UNPUBLISH].is_object());
    }

    #[test]
    fn validate_ignores_values_when_neither_key_is_posted() {
        let result = __inner_tap_form_validate(serde_json::json!({
            "form_id": "item_edit_blog",
            "values": {"title": "x"},
            "item_id": null,
        }));
        assert_eq!(result, serde_json::json!({}));
    }

    #[test]
    fn validate_refuses_a_past_date_on_the_json_item_api() {
        let result = __inner_tap_form_validate(serde_json::json!({
            "form_id": "json_item_blog",
            "values": {"field_publish_on": 1},
            "item_id": null,
        }));
        assert_eq!(result["errors"][0]["field"], "field_publish_on");
    }

    #[test]
    fn submit_ignores_a_form_with_neither_key_posted() {
        // With the native stub, `execute_raw` always returns `Ok(0)`, so a
        // call would not panic either way; this proves the short circuit by
        // being here at all rather than by observing a side effect the
        // stub cannot show.
        let result = __inner_tap_form_submit(serde_json::json!({
            "form_id": "item_edit_blog",
            "values": {"title": "x"},
            "item_id": "11111111-1111-1111-1111-111111111111",
        }));
        assert_eq!(result, serde_json::json!({}));
    }
}
