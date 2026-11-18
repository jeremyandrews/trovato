//! B68 test fixture: a plugin bulk action.
//!
//! Declares `stamp` under `[[bulk_actions]]`. `tap_bulk_action` writes one row
//! to `tba_log` per item id it is handed, with the action name and the user the
//! chunk runs as, and reports every item as succeeded. The suite reads the log
//! to see which items reached the plugin and under whose authority.

use serde_json::{Value, json};
use trovato_sdk::plugin_tap;

#[plugin_tap]
fn tap_bulk_action(input: Value) -> Value {
    let action = input.get("action").and_then(Value::as_str).unwrap_or("");
    let user = trovato_sdk::host::current_user_id();
    let ids = input
        .get("item_ids")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for id in &ids {
        let _ = trovato_sdk::host::execute_raw(
            "INSERT INTO tba_log (item_id, action, user_id) VALUES ($1, $2, $3)",
            &[id.clone(), json!(action), json!(user)],
        );
    }
    json!({ "succeeded": ids.len(), "failed": 0 })
}
