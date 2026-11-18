//! B68 test fixture: a plugin that pushes to a queue another plugin owns.
//!
//! `tap_cron` takes `{"owner", "queue", "payload", "count"}`, pushes `payload`
//! to `owner`'s `queue` `count` times (default 1) through `push-to`, each push
//! numbered under `n` so no two are the same job, and
//! returns every code the kernel answered, so the suite can tell an accepted
//! push from a refused one.

use serde_json::{Value, json};
use trovato_sdk::plugin_tap;

mod push;

#[plugin_tap]
fn tap_cron(input: Value) -> Value {
    let owner = input.get("owner").and_then(Value::as_str).unwrap_or("");
    let queue = input.get("queue").and_then(Value::as_str).unwrap_or("");
    let payload = input.get("payload").cloned().unwrap_or(json!({}));
    let count = input.get("count").and_then(Value::as_u64).unwrap_or(1);
    let codes: Vec<i32> = (0..count)
        .map(|n| {
            let mut payload = payload.clone();
            if let Some(object) = payload.as_object_mut() {
                object.insert("n".to_string(), json!(n));
            }
            payload
        })
        .map(|payload| match push::push_to(owner, queue, &payload) {
            Ok(()) => 0,
            Err(code) => code,
        })
        .collect();
    json!({ "codes": codes })
}
