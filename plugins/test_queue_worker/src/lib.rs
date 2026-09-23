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
//!   `"spin"` burns CPU and never returns, so only the epoch deadline ends it —
//!   the shape that held a worker slot for the whole budget and was then handed
//!   the budget again on the next cycle;
//!   `"error"` returns an error-shaped JSON body (a *successful* dispatch under
//!   the drain's contract — proving error-JSON is not retried, preserving the
//!   reference importer's semantics); `"mail"` calls the `mail` host interface
//!   and reports the code it got back, so the suite can see what a plugin
//!   sending mail from a queue worker is actually told; anything else returns
//!   success.
//! - `tap_cron` enqueues two jobs through the additive `enqueue` host function
//!   (one high-priority, one delayed) so the suite can verify priority and
//!   delay reach `plugin_queue`.

use serde_json::json;
use trovato_sdk::plugin_tap;
use trovato_sdk::types::QueueOptions;

/// Logical queue name owned by this fixture.
const QUEUE_NAME: &str = "test_queue";

/// A second queue owned by this fixture, declared at concurrency **1**.
///
/// Its whole purpose is to differ from [`QUEUE_NAME`]'s declaration, so the
/// suite can tell a per-queue width from a per-plugin one. A plugin declaring a
/// wide queue and a narrow one is the shape that exposed the collapse: the
/// narrow queue used to be drained at the wide queue's width.
const SERIAL_QUEUE_NAME: &str = "test_serial_queue";

/// Declare both queues this fixture owns: one at concurrency 8 (clamped to the
/// kernel cap of 4 by the drain — D-47) and one at concurrency 1.
#[plugin_tap]
fn tap_queue_info() -> serde_json::Value {
    json!([
        {
            "name": QUEUE_NAME,
            "concurrency": 8
        },
        {
            "name": SERIAL_QUEUE_NAME,
            "concurrency": 1
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
/// - `"mail"` → call the `mail` host interface and return whatever it said. The
///   code is reported rather than swallowed so the suite can tell a rate-limit
///   refusal from any other, which is the whole point of the per-plugin mail
///   bucket applying on this path.
#[plugin_tap]
fn tap_queue_worker(input: serde_json::Value) -> serde_json::Value {
    match input.get("outcome").and_then(|v| v.as_str()) {
        Some("trap") => panic!("test_queue_worker: intentional trap"),
        Some("spin") => {
            // A guest that burns CPU and never returns. The kernel's only bound
            // on this is the epoch deadline, so a drain that dispatches it is
            // held for the whole epoch budget with no way to reclaim the slot.
            loop {
                std::hint::spin_loop();
            }
        }
        Some("error") => json!({ "status": "error", "reason": "intentional" }),
        Some("mail") => {
            let code = match trovato_sdk::host::mail_send_to_site_contacts(
                "queue worker mail",
                "sent from tap_queue_worker",
                &[],
            ) {
                Ok(()) => 0,
                Err(code) => code,
            };
            // The drain deletes a succeeding job and keeps no record of what it
            // returned, so a payload that states the expected code makes the
            // answer observable: match and the job succeeds, mismatch and this
            // traps, which the drain records as a failed attempt.
            if let Some(expected) = input.get("expect_code").and_then(|v| v.as_i64())
                && i64::from(code) != expected
            {
                panic!("test_queue_worker: mail returned {code}, expected {expected}");
            }
            json!({ "status": "ok", "mail_code": code })
        }
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
