//! B278 test fixture: precise timers.
//!
//! `tap_timer` reports each fire to the `fired.<name>` variable as
//! `{ "name", "run_at", "fired_at", "attempt", "probe" }`, so the suite can
//! read back what the kernel handed it. The payload steers what else it does:
//!
//! - `{ "probe": "<name>" }`: read and cancel the timer `<name>` through the
//!   `timer` host interface, and report what came back. Pointed at another
//!   plugin's timer it must see nothing and cancel nothing.
//! - `{ "set": { ...TimerRequest } }`: set one of its own timers, and report
//!   the answer (`0`, or an error code).
//! - `{ "fail": true }`: fail the attempt with an `error` object.

use serde_json::{Value, json};
use trovato_sdk::host;
use trovato_sdk::plugin_tap;
use trovato_sdk::types::{TimerInput, TimerRequest};

#[plugin_tap]
fn tap_timer(input: TimerInput) -> Value {
    let mut report = json!({
        "name": input.name,
        "run_at": input.run_at,
        "fired_at": input.fired_at,
        "attempt": input.attempt,
    });
    if let Some(target) = input.payload.get("probe").and_then(Value::as_str) {
        let seen = match host::timer_get(target) {
            Ok(Some(t)) => json!(t.name),
            Ok(None) => Value::Null,
            Err(code) => json!(code),
        };
        let cancelled = match host::timer_cancel(target) {
            Ok(was) => json!(was),
            Err(code) => json!(code),
        };
        report["probe"] = json!({ "seen": seen, "cancelled": cancelled });
    }
    if let Some(request) = input.payload.get("set") {
        let answer = match serde_json::from_value::<TimerRequest>(request.clone()) {
            Ok(request) => match host::timer_set(&request) {
                Ok(()) => 0,
                Err(code) => code,
            },
            Err(_) => -14,
        };
        report["set"] = json!(answer);
    }
    let _ = host::variables_set(&format!("fired.{}", input.name), &report.to_string());
    if input.payload.get("fail").and_then(Value::as_bool) == Some(true) {
        return json!({ "error": format!("asked to fail on attempt {}", input.attempt) });
    }
    report
}
