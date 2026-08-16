//! P11d end-to-end test fixture: the **plugin queue v2** (D-45..D-48).
//!
//! Built with the real `trovato-plugin-sdk` and the real `wasm32-wasip1`
//! toolchain — the same path a plugin author uses — so the P11d integration
//! suite exercises the v2 drain (claim locking, backoff, retry accounting,
//! dead-lettering), the honored-concurrency parse, and the additive `enqueue`
//! host function through genuine SDK-compiled WASM rather than in-test stubs.
//!
//! Three taps:
//!
//! - `tap_queue_info` declares one queue (`test_queue`) at **concurrency 8** so
//!   the suite can assert the kernel clamps it to the cap of 4.
//! - `tap_queue_worker` branches on the payload's `outcome` field:
//!   `"trap"` panics (a WASM trap → a *failed attempt*, not a lost item);
//!   `"error"` returns an error-shaped JSON body (a *successful* dispatch under
//!   the drain's contract — proving error-JSON is not retried, preserving the
//!   reference importer's semantics); anything else returns success.
//! - `tap_cron` enqueues two jobs through the additive `enqueue` host function
//!   (one high-priority, one delayed) so the suite can verify priority and
//!   delay reach `plugin_queue`.

use serde_json::json;
use trovato_sdk::plugin_tap;
use trovato_sdk::types::QueueOptions;

/// Logical queue name owned by this fixture.
const QUEUE_NAME: &str = "test_queue";

/// Declare the queue this fixture owns at concurrency 8 (clamped to the kernel
/// cap of 4 by the drain — D-47).
#[plugin_tap]
fn tap_queue_info() -> serde_json::Value {
    json!([
        {
            "name": QUEUE_NAME,
            "concurrency": 8
        }
    ])
}

/// Process one queued job. Behavior is driven by the payload's `outcome`:
///
/// - `"trap"` → panic, producing a WASM trap the kernel counts as a failed
///   attempt (rescheduled with backoff, then dead-lettered at `max_attempts`);
/// - `"error"` → return an error-shaped JSON body; this is still a *successful*
///   dispatch (positive-length output), so the drain deletes it — matching how
///   the reference importer's `{"status":"error"}` returns behave;
/// - anything else → succeed.
#[plugin_tap]
fn tap_queue_worker(input: serde_json::Value) -> serde_json::Value {
    match input.get("outcome").and_then(|v| v.as_str()) {
        Some("trap") => panic!("test_queue_worker: intentional trap"),
        Some("error") => json!({ "status": "error", "reason": "intentional" }),
        _ => json!({ "status": "ok" }),
    }
}

/// Enqueue two jobs through the additive `enqueue` host function (D-48): one at
/// high priority with no delay, one at default priority deferred by an hour.
/// Lets the suite verify `priority`/`delay` reach `plugin_queue`.
#[plugin_tap]
fn tap_cron(_input: serde_json::Value) -> serde_json::Value {
    let _ = trovato_sdk::host::queue_enqueue(
        QUEUE_NAME,
        &json!({ "outcome": "ok", "tag": "priority" }),
        &QueueOptions {
            priority: 10,
            delay: 0,
        },
    );
    let _ = trovato_sdk::host::queue_enqueue(
        QUEUE_NAME,
        &json!({ "outcome": "ok", "tag": "delayed" }),
        &QueueOptions {
            priority: 0,
            delay: 3600,
        },
    );
    json!({ "status": "ok" })
}
