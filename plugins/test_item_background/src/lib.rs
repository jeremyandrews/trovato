//! U40 test fixture: `item-api` writes and `query` reads from the two
//! background paths.
//!
//! `tap_cron` and `tap_queue_worker` dispatch under the kernel's background
//! principal, through services `CronService` builds for itself. This fixture
//! makes one `item-api` or `query` call per dispatch from each, so the suite
//! can drive the real cron cycle and the real queue drain and then look at the
//! `item` table.
//!
//! A step is a JSON object: `{ "nonce", "op", "type", "title", "id" }`, where
//! `op` is `create`, `update`, `get`, `delete` or `query` (a
//! `get` of no item answers an empty title). A `query` runs a Gather
//! definition over items of `type` titled `title` and answers the first row's
//! id and title, plus `total`, the rows the viewer may see. `tap_cron` takes
//! its step from the `step` variable and clears it, so a later cycle does
//! nothing; the queue worker takes its step as the job payload. Each reports
//! what the host said to the `cron_result` or `queue_result` variable, tagged
//! with the step's nonce: `{ "nonce", "op", "code", "id", "title" }`, `code`
//! being `0` or the host's error code.

use serde_json::{Value, json};
use trovato_sdk::host;
use trovato_sdk::plugin_tap;

/// Run one step and say what happened.
fn perform(step: &Value) -> Value {
    let nonce = step.get("nonce").cloned().unwrap_or(Value::Null);
    let op = step.get("op").and_then(Value::as_str).unwrap_or("");
    let text = |key: &str| step.get(key).and_then(Value::as_str).unwrap_or("");
    let answer = |code: i32, id: &str, title: &str| json!({ "nonce": nonce, "op": op, "code": code, "id": id, "title": title });
    match op {
        "create" => {
            match host::save_item(&json!({ "type": text("type"), "title": text("title") })) {
                Ok(Some(item)) => answer(0, &item.id.to_string(), &item.title),
                Ok(None) => answer(0, "", ""),
                Err(code) => answer(code, "", ""),
            }
        }
        "update" => match host::save_item(&json!({ "id": text("id"), "title": text("title") })) {
            Ok(Some(item)) => answer(0, &item.id.to_string(), &item.title),
            Ok(None) => answer(0, "", ""),
            Err(code) => answer(code, text("id"), ""),
        },
        "get" => match host::get_item(text("id")) {
            Ok(Some(item)) => answer(0, &item.id.to_string(), &item.title),
            Ok(None) => answer(0, text("id"), ""),
            Err(code) => answer(code, text("id"), ""),
        },
        "delete" => match host::delete_item(text("id")) {
            Ok(()) => answer(0, text("id"), ""),
            Err(code) => answer(code, text("id"), ""),
        },
        "query" => {
            let definition = json!({
                "base_table": "item",
                "item_type": text("type"),
                "filters": [{ "field": "title", "operator": "equals", "value": text("title") }]
            });
            match host::query(&definition, &json!({})) {
                Ok(result) => {
                    let first = result.items.first();
                    let field = |key: &str| {
                        first
                            .and_then(|row| row.get(key))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string()
                    };
                    let mut reply = answer(0, &field("id"), &field("title"));
                    reply["total"] = json!(result.total);
                    reply
                }
                Err(code) => answer(code, "", ""),
            }
        }
        _ => answer(0, "", ""),
    }
}

/// Report a step's answer; best effort, the suite waits on it.
fn report(variable: &str, answer: &Value) {
    let _ = host::variables_set(variable, &answer.to_string());
}

/// Run the pending step, if there is one.
#[plugin_tap]
fn tap_cron(_input: Value) -> Value {
    let raw = host::variables_get("step", "").unwrap_or_default();
    let Ok(step) = serde_json::from_str::<Value>(&raw) else {
        return json!({});
    };
    if !step.is_object() {
        return json!({});
    }
    let _ = host::variables_set("step", "");
    let answer = perform(&step);
    report("cron_result", &answer);
    json!({})
}

/// Run the job's step.
#[plugin_tap]
fn tap_queue_worker(input: Value) -> Value {
    let answer = perform(&input);
    report("queue_result", &answer);
    json!({ "status": "ok" })
}
