//! B68 test fixture: the owner of a shared queue.
//!
//! - `tap_queue_info` declares three queues: `shared` at concurrency 2, shared
//!   with `test_queue_sender`; `private`, shared with nobody; and
//!   `shared_elsewhere`, shared with a plugin that is not `test_queue_sender`.
//! - `tap_queue_worker` writes the input it received to `tqo_log`, with the
//!   time it started and the time it finished, after holding the job for the
//!   payload's `hold_ms` (default 0). The suite reads the input to see who the
//!   kernel says sent the job, and the times to see how many ran at once. The
//!   sender numbers each push, so every input is distinct.

use serde_json::{Value, json};
use trovato_sdk::plugin_tap;

#[plugin_tap]
fn tap_queue_info() -> Value {
    json!([
        { "name": "shared", "concurrency": 2, "shared_with": ["test_queue_sender"] },
        { "name": "private", "concurrency": 1 },
        { "name": "shared_elsewhere", "concurrency": 1, "shared_with": ["someone_else"] }
    ])
}

/// Record the job, holding it first if the payload asks.
#[plugin_tap]
fn tap_queue_worker(input: Value) -> Value {
    // A shared job carries its payload under `payload`; an owner's own job is
    // the payload.
    let payload = input.get("payload").unwrap_or(&input);
    let hold_ms = payload.get("hold_ms").and_then(Value::as_i64).unwrap_or(0);
    let _ = trovato_sdk::host::execute_raw(
        "INSERT INTO tqo_log (input, started) VALUES ($1, clock_timestamp())",
        &[json!(input.to_string())],
    );
    if hold_ms > 0 {
        let seconds = hold_ms as f64 / 1000.0;
        let _ = trovato_sdk::host::query_raw(&format!("SELECT pg_sleep({seconds})"), &[]);
    }
    let _ = trovato_sdk::host::execute_raw(
        "UPDATE tqo_log SET finished = clock_timestamp() \
         WHERE input = $1 AND finished IS NULL",
        &[json!(input.to_string())],
    );
    json!({ "status": "ok" })
}
