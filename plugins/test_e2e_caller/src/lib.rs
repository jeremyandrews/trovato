//! Epic 2 end-to-end test fixture: the **caller** side of plugin-to-plugin
//! invocation (FR-4a).
//!
//! Built with the real `trovato-plugin-sdk` and the real `wasm32-wasip1`
//! toolchain — the same path a plugin author uses — so the Story 2.4 integration
//! suite exercises the invocation surface through genuine SDK-compiled WASM rather
//! than in-test WAT fixtures.
//!
//! This plugin declares `host_interfaces = ["logging", "plugin-api"]`, so the
//! per-plugin linker (WASM-1) grants it the `plugin-api` imports. Its single tap,
//! `tap_cron`, is a generic **invocation driver**: the integration test dispatches
//! it a JSON command describing which target/function/payload to invoke (or a
//! `plugin-exists` probe), and it returns the outcome as JSON. Driving through a
//! real tap dispatch keeps the whole path real: caller WASM → SDK `invoke()` →
//! host `invoke` → callee WASM.

use serde::Deserialize;
use serde_json::json;
use trovato_sdk::plugin_tap;

/// A command from the integration test telling the driver what to invoke.
///
/// `function == "@exists"` is a sentinel: instead of `invoke`, the driver calls
/// `plugin_exists(target)` and returns `{"exists": bool}`. Otherwise it calls
/// `invoke(target, function, payload)`.
#[derive(Deserialize)]
struct DriverCommand {
    /// Target plugin machine name.
    target: String,
    /// Function to invoke on the target, or the `@exists` sentinel.
    function: String,
    /// Literal payload passed straight to `invoke` (default empty).
    #[serde(default)]
    payload: String,
    /// When set, the driver builds the payload internally as `n` `'a'` bytes
    /// rather than using `payload`. This lets the test drive the >1 MiB inbound
    /// payload case without pushing a 1 MiB string through the 64 KiB tap-input
    /// buffer (`call_export_function` caps tap input at 64 KiB).
    #[serde(default)]
    payload_repeat: Option<usize>,
}

/// Invocation driver, invoked by the Story 2.4 suite via `TapDispatcher`.
///
/// Returns `{"ok": <result>}` / `{"err": <error>}` for an invoke, or
/// `{"exists": <bool>}` for the `@exists` probe. The error branch carries the
/// frozen `invoke` error string verbatim, so the test can assert exact prefixes.
#[plugin_tap]
fn tap_cron(cmd: DriverCommand) -> serde_json::Value {
    if cmd.function == "@exists" {
        return json!({ "exists": trovato_sdk::host::plugin_exists(&cmd.target) });
    }

    let payload = match cmd.payload_repeat {
        Some(n) => "a".repeat(n),
        None => cmd.payload,
    };

    match trovato_sdk::host::invoke(&cmd.target, &cmd.function, &payload) {
        Ok(result) => json!({ "ok": result }),
        Err(error) => json!({ "err": error }),
    }
}
