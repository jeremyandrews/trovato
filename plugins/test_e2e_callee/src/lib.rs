//! Epic 2 end-to-end test fixture: the **callee** side of plugin-to-plugin
//! invocation (FR-4a), plus the sandbox-composition probes for Story 2.4.
//!
//! Built with the real `trovato-plugin-sdk` and the real `wasm32-wasip1`
//! toolchain. The manifest publishes exactly the functions the integration suite
//! reaches through `invoke`; `secret` is deliberately *unpublished* (present as a
//! WASM export but absent from `public_functions`) so the callee-consent gate can
//! be exercised, and `not_exported` is deliberately published-but-missing.
//!
//! # Export shapes
//!
//! Invocable functions use the same `(ptr, len) -> i64` JSON memory protocol as
//! taps. Most reuse the SDK's `#[plugin_tap]` macro (the SDK's only export
//! generator today — it emits exactly that ABI). Two functions hand-write the
//! export because they must sidestep the macro:
//!
//! - `big_out` returns a result one byte over the frozen 1 MiB cap; the macro
//!   caps its output buffer at 64 KiB, so it cannot produce an over-cap result.
//! - `recurse` returns the inner `invoke` result **verbatim** (raw bytes). Going
//!   through the macro would JSON-encode the string at every frame, compounding
//!   quotes across the recursion chain and corrupting the frozen error message
//!   before it reached the top. The raw passthrough keeps it byte-exact.

use serde_json::json;
use trovato_sdk::plugin_tap;

/// Published. Echoes its JSON input straight back, so the suite can prove a
/// payload round-trips intact across the invoke boundary.
#[plugin_tap]
fn echo(input: serde_json::Value) -> serde_json::Value {
    input
}

/// **Unpublished** (exported, but not in `public_functions`). Invoking it must be
/// rejected by the callee-consent gate with `permission-denied`.
#[plugin_tap]
fn secret(_input: serde_json::Value) -> serde_json::Value {
    json!({ "secret": true })
}

/// Published. Attempts a raw-SQL read through the SDK (`query_raw`) — the only
/// database path the SDK exposes. This callee does not declare `raw_sql`, so the
/// WASM-2 gate rejects the call with `ERR_RAW_SQL_NOT_DECLARED` (-17) before any
/// pool access; the numeric code is returned so the caller can observe that the
/// sandbox fired during the nested invoke. (The structured `table-not-declared`
/// path has no SDK binding — see the integration test's notes.)
#[plugin_tap]
fn read_undeclared(_input: serde_json::Value) -> serde_json::Value {
    match trovato_sdk::host::query_raw("SELECT 1 FROM undeclared_table", &[]) {
        Ok(rows) => json!({ "db_ok": rows }),
        Err(code) => json!({ "db_code": code }),
    }
}

/// Published. Allocates far more linear memory than a small per-`Store` limiter
/// cap allows, so the WASM-4 limiter traps the call. The trap surfaces to the
/// caller as `target-errored` (a limiter breach is a logged, attributed trap,
/// never the kernel). Only invoked under a deliberately small memory cap.
#[plugin_tap]
fn greedy_mem(_input: serde_json::Value) -> serde_json::Value {
    // 64 MiB, well above the small cap the greedy-callee test configures. `resize`
    // forces the allocation and touches the pages so `memory.grow` is driven past
    // the cap. Never returns: the limiter traps mid-growth.
    let mut hog: Vec<u8> = Vec::new();
    hog.resize(64 * 1024 * 1024, 0xAB);
    std::hint::black_box(&hog);
    json!({ "unreachable": hog.len() })
}

/// Published (hand-written export — see the module docs). Returns a result one
/// byte over the frozen 1 MiB payload cap, so the host's outbound check rejects
/// the invoke with `payload-too-large`.
///
/// # Safety
///
/// Uses the raw `(ptr, len) -> i64` export ABI. The returned buffer is leaked so
/// it stays valid in this `Store`'s memory until the host reads it synchronously
/// after the call returns.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn big_out(_ptr: i32, _len: i32) -> i64 {
    // MAX_PAYLOAD_BYTES + 1. All-'a' so the host's UTF-8 decode of the result
    // succeeds and the *size* check (not a decode error) is what rejects it.
    const OVER_CAP: usize = 1_048_576 + 1;
    let buf: &'static mut [u8] = vec![b'a'; OVER_CAP].leak();
    let ptr = buf.as_mut_ptr() as i64;
    (ptr << 32) | (OVER_CAP as i64)
}

/// Published (hand-written export — see the module docs). Self-invokes
/// `test_e2e_callee::recurse`, then returns the inner result's raw bytes verbatim
/// (Ok or Err). Driven by the caller, this builds a genuine N-deep chain across
/// real `Store` boundaries; the innermost `recursion-limit-exceeded` message
/// propagates up unchanged so the suite can assert the exact frozen boundary.
///
/// # Safety
///
/// Uses the raw `(ptr, len) -> i64` export ABI; the returned buffer is leaked so
/// it stays valid until the host reads it synchronously after the call returns.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn recurse(_ptr: i32, _len: i32) -> i64 {
    let inner = match trovato_sdk::host::invoke("test_e2e_callee", "recurse", "{}") {
        Ok(result) => result,
        Err(error) => error,
    };
    let buf: &'static mut [u8] = inner.into_bytes().leak();
    let ptr = buf.as_mut_ptr() as i64;
    let len = buf.len() as i64;
    (ptr << 32) | len
}
