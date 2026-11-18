//! B68 test fixture: a plugin that watches item writes and makes its own.
//!
//! Three jobs, each one a thing the item-api suite has to observe from inside a
//! real plugin built with the real SDK:
//!
//! - **Witness.** Every item write tap (`tap_item_presave`, `tap_item_insert`,
//!   `tap_item_update`, `tap_item_update_index`, `tap_item_delete`) writes one
//!   row to `tiw_log` naming the tap and the item. A write some *other* plugin
//!   makes through `save-item` or `delete-item` therefore shows up here exactly
//!   when it goes through `ItemService`, which is what fires these taps.
//! - **Echo.** `tap_item_update` on an item whose title starts with `echo`
//!   rewrites that same item through `save-item`, and logs the host's answer.
//!   Unbounded, that is a write whose taps fire a write whose taps fire a
//!   write; the suite asserts it terminates.
//! - **Driver.** `tap_queue_worker` runs one item-api round trip (create, get,
//!   query, update, delete, delete again) and returns every answer, so the
//!   suite can check the SDK bindings against the host functions end to end.

use serde_json::{Value, json};
use trovato_sdk::plugin_tap;

mod items;

/// Record one tap in `tiw_log`. Best effort: a failed insert must not change
/// what the tap returns, because the suite reads the log, not the return.
fn record(tap: &str, item: &Value, note: &str) {
    let id = item.get("id").and_then(Value::as_str).unwrap_or("");
    let title = item.get("title").and_then(Value::as_str).unwrap_or("");
    let _ = trovato_sdk::host::execute_raw(
        "INSERT INTO tiw_log (tap, item_id, title, note) VALUES ($1, $2, $3, $4)",
        &[json!(tap), json!(id), json!(title), json!(note)],
    );
}

/// Presave carries no id on a create, only the title, so the suite matches it
/// by title.
#[plugin_tap]
fn tap_item_presave(input: Value) -> Value {
    record("presave", &input, "");
    json!({})
}

#[plugin_tap]
fn tap_item_insert(input: Value) -> Value {
    record("insert", &input, "");
    json!({})
}

#[plugin_tap]
fn tap_item_update_index(input: Value) -> Value {
    record("update_index", &input, "");
    json!({})
}

#[plugin_tap]
fn tap_item_delete(input: Value) -> Value {
    record("delete", &input, "");
    json!({})
}

/// Record the update; on an `echo` title, write the same item again.
#[plugin_tap]
fn tap_item_update(input: Value) -> Value {
    record("update", &input, "");
    let title = input.get("title").and_then(Value::as_str).unwrap_or("");
    if title.starts_with("echo")
        && let Some(id) = input.get("id").and_then(Value::as_str)
    {
        let answer = match items::save_item(&json!({ "id": id, "title": format!("{title}+") })) {
            Ok(Some(_)) => "saved".to_string(),
            Ok(None) => "missing".to_string(),
            Err(code) => code.to_string(),
        };
        record("echo", &input, &answer);
    }
    json!({})
}

/// One item-api round trip, every answer reported.
///
/// Input: `{ "type": "<item type>", "title": "<title>" }`.
#[plugin_tap]
fn tap_queue_worker(input: Value) -> Value {
    let item_type = input.get("type").and_then(Value::as_str).unwrap_or("page");
    let title = input
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("round trip");

    let created = match items::save_item(&json!({ "type": item_type, "title": title })) {
        Ok(Some(item)) => item,
        Ok(None) => return json!({ "create": "missing" }),
        Err(code) => return json!({ "create": code }),
    };
    let id = created.id.to_string();

    let got = match items::get_item(&id) {
        Ok(Some(item)) => json!(item.title),
        Ok(None) => json!(null),
        Err(code) => json!(code),
    };
    let listed = match items::query_items(&json!({ "type": item_type, "limit": 100 })) {
        Ok(found) => json!(found.iter().any(|i| i.id == created.id)),
        Err(code) => json!(code),
    };
    let updated = match items::save_item(&json!({ "id": id, "title": format!("{title} (edited)") }))
    {
        Ok(Some(item)) => json!(item.title),
        Ok(None) => json!(null),
        Err(code) => json!(code),
    };
    let deleted = match items::delete_item(&id) {
        Ok(()) => json!(0),
        Err(code) => json!(code),
    };
    let deleted_again = match items::delete_item(&id) {
        Ok(()) => json!(0),
        Err(code) => json!(code),
    };
    let got_after = match items::get_item(&id) {
        Ok(Some(_)) => json!("present"),
        Ok(None) => json!(null),
        Err(code) => json!(code),
    };

    json!({
        "id": id,
        "created_title": created.title,
        "get": got,
        "query_found": listed,
        "update": updated,
        "delete": deleted,
        "delete_again": deleted_again,
        "get_after_delete": got_after,
    })
}
