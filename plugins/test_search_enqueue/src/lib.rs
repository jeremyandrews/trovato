//! B218 test fixture: the plugin rule for the static search index.
//!
//! A raw write to an item is queued by the database; a plugin that changes
//! anything else a search page depends on queues the item with
//! `host::search_index_enqueue`, and the queue row then names it. `tap_cron`
//! takes a step from the `step` variable, `{ "nonce", "id", "title",
//! "enqueue" }`, clears it, retitles the item with a raw `UPDATE item`, and
//! queues it when `enqueue` is true. It reports `{ "nonce", "updated",
//! "queued" }` to the `result` variable, `queued` being the host's answer
//! (rows queued, or a negative error code).

use serde_json::{Value, json};
use trovato_sdk::host;
use trovato_sdk::plugin_tap;

/// Run the pending step, if there is one.
#[plugin_tap]
fn tap_cron(_input: Value) -> Value {
    let Ok(raw) = host::variables_get("step", "") else {
        return json!({});
    };
    let Ok(step) = serde_json::from_str::<Value>(&raw) else {
        return json!({});
    };
    let _ = host::variables_set("step", "");
    let text = |key: &str| {
        step.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let id = text("id");
    let updated = host::execute_raw(
        "UPDATE item SET title = $1 WHERE id = $2::uuid",
        &[json!(text("title")), json!(id)],
    )
    .map_or_else(i64::from, |n| n as i64);
    let queued = if step
        .get("enqueue")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        host::search_index_enqueue(&[id.as_str()]).map_or_else(i64::from, |n| n as i64)
    } else {
        0
    };
    let answer = json!({ "nonce": step.get("nonce"), "updated": updated, "queued": queued });
    let _ = host::variables_set("result", &answer.to_string());
    answer
}
