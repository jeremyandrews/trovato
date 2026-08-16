//! Plugin-to-plugin invocation host functions (FR-4a).
//!
//! Implements the host side of the `trovato:kernel/plugin-api` WIT interface —
//! `invoke` and `plugin-exists` — under a callee-side security model: the
//! target plugin declares a public-export allowlist in its manifest, and
//! nothing outside that list is reachable. The full model is documented below.
//!
//! # Permission model
//!
//! - **Callee gate (enforced here):** a function is invocable only if the target
//!   plugin lists it in `[capabilities].public_functions`. Deny-by-default:
//!   `capabilities: None`, an absent list, or an empty list all expose no
//!   invocable surface (a deliberate divergence from the `host_interfaces`
//!   None-grants-all default — invocation has no pre-existing consumers to
//!   preserve).
//! - **Caller gate (NOT enforced yet):** the design relies on the WASM-1
//!   per-plugin linker (D-18) to gate whether a plugin even receives the
//!   `plugin-api` imports (`host_interfaces = ["plugin-api"]`). Until WASM-1
//!   lands, the shared linker grants every plugin every host import, so the
//!   caller side is open. This is acceptable for 1.0 (no plugin invokes another
//!   today; the irreversible callee consent gate ships here), but it is **not**
//!   complete caller-side enforcement.
//! - **No user/role awareness:** `invoke` is plugin-to-plugin trust. The target
//!   function receives this request's `RequestState` (same user/context/services)
//!   and performs its own user-level checks.
//!
//! # Recursion
//!
//! The chain is bounded by a kernel-owned depth counter (Story 2.3). The
//! originating request runs at depth `0`; [`do_invoke`] executes one level deeper
//! than its caller, carrying `parent + 1` in the child's cloned [`RequestState`]
//! (Story 2.2 clones state per call, so there is no shared counter to decrement —
//! each frame owns its clone). A dispatch that would reach
//! [`MAX_INVOCATION_DEPTH`] is rejected before any instantiation with
//! [`ERR_RECURSION_LIMIT`], so an over-deep chain stops cleanly with no partial
//! commit. Plugins cannot read or reset the counter: it lives in `RequestState`
//! (host-side) and no host function exposes it.
//!
//! # ABI note
//!
//! The WIT contract is `invoke -> result<string, string>` — the error channel is
//! a *string*, not a numeric code, so the standard
//! [`host_errors`](trovato_sdk::host_errors) negative-code convention does not
//! apply. Instead `invoke` returns a length-tagged `i64` and carries the Ok or
//! Err string in the caller's output buffer:
//!
//! - `r >= 0`: success; `out[0..r]` is the UTF-8 result string.
//! - `r < 0`:  failure; let `n = (-r) - 1`; `out[0..n]` is the UTF-8 error string
//!   (always beginning with one of the frozen kebab prefixes below).
//!
//! `plugin-exists -> bool` returns `1`/`0` as an `i32`.

use anyhow::Result;
use wasmtime::{Caller, Extern, Linker, Memory};

use super::{read_string_from_memory, write_string_to_memory};
use crate::plugin::{PluginCapabilities, PluginRuntime, PluginState, WasmtimeExt};
use crate::tap::{ExportCallError, RequestState, instantiate_and_call_export};

/// Maximum size (bytes) of an `invoke` payload or result string (frozen: 1 MiB).
///
/// Applied to both the inbound `payload` and the outbound `result`; overflow in
/// either direction yields [`ERR_PAYLOAD_TOO_LARGE`]. The *existence* of the cap
/// and its error shape freeze at FR-4; the numeric value is tunable upward.
pub(crate) const MAX_PAYLOAD_BYTES: usize = 1_048_576;

/// Per-call epoch budget (seconds) for an invoked target function — the same
/// request-scoped deadline tap dispatch uses for non-background taps.
///
/// The value lives in the shared resource-limits home (WASM-4) so all resource
/// bounds sit in one place; re-exported here under the local name callers use.
use crate::plugin::limits::INVOKE_EPOCH_DEADLINE_SECS as INVOKE_EPOCH_DEADLINE;

/// Maximum plugin-to-plugin invocation chain depth (Jeremy-approved cap, 2026-06-04).
///
/// The originating request executes at depth `0`; each `invoke` runs one level
/// deeper. [`do_invoke`] computes the depth this dispatch *would* execute at
/// (`parent + 1`) and rejects when it reaches this cap (`depth >= MAX`). With that
/// boundary, up to `MAX_INVOCATION_DEPTH - 1` (= 7) nested invocations succeed and
/// the next is rejected at `depth 8 >= 8` — i.e. the chain is at most
/// `MAX_INVOCATION_DEPTH` frames (the origin plus 7 invokes). Legitimate
/// cross-plugin composition is shallow (A→B→C), so this leaves generous headroom
/// while bounding stack growth and the number of concurrently-live `Store`s.
///
/// Frozen *shape*, tunable *value*: like [`MAX_PAYLOAD_BYTES`], the existence of
/// the cap and its [`ERR_RECURSION_LIMIT`] error shape freeze at FR-4, but the
/// numeric value does not — a future kernel may raise it without a contract change
/// (lowering it could break callers and would be a contract change).
pub(crate) const MAX_INVOCATION_DEPTH: u32 = 8;

// --- Frozen error vocabulary (design §3.3) --------------------------------
// Each `Err(string)` begins with one of these stable kebab prefixes + ": ".
// Plugins branch on the prefix; the suffix is informational human detail.

/// No installed + enabled plugin by that name.
pub(crate) const ERR_TARGET_NOT_FOUND: &str = "target-not-found";
/// Function not in the target's `public_functions` (callee consent denied).
pub(crate) const ERR_PERMISSION_DENIED: &str = "permission-denied";
/// Declared public but the WASM export is missing (manifest/binary drift).
pub(crate) const ERR_FUNCTION_NOT_EXPORTED: &str = "function-not-exported";
/// The target function ran and returned an error or trapped.
pub(crate) const ERR_TARGET_ERRORED: &str = "target-errored";
/// Inbound or outbound string exceeds [`MAX_PAYLOAD_BYTES`].
pub(crate) const ERR_PAYLOAD_TOO_LARGE: &str = "payload-too-large";
/// Invocation chain reached [`MAX_INVOCATION_DEPTH`] (Story 2.3). Emitted by
/// [`do_invoke`] before any dispatch when `parent_depth + 1 >= MAX`.
pub(crate) const ERR_RECURSION_LIMIT: &str = "recursion-limit-exceeded";

/// Register the plugin-to-plugin invocation host functions.
///
/// Provides `invoke` and `plugin-exists` under `trovato:kernel/plugin-api`,
/// matching the WIT identifiers (kebab-case host fields, per PF-3 / D-21).
pub fn register_plugin_api_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // invoke(plugin_ptr, plugin_len, fn_ptr, fn_len, payload_ptr, payload_len,
    //        out_ptr, out_max_len) -> i64  (length-tagged; see module ABI note)
    linker
        .func_wrap_async(
            "trovato:kernel/plugin-api",
            "invoke",
            |mut caller: Caller<'_, PluginState>,
             (
                plugin_ptr,
                plugin_len,
                fn_ptr,
                fn_len,
                payload_ptr,
                payload_len,
                out_ptr,
                out_max_len,
            ): (i32, i32, i32, i32, i32, i32, i32, i32)| {
                Box::new(async move {
                    let Some(Extern::Memory(memory)) = caller.get_export("memory") else {
                        // No memory export: a fundamentally broken module (cannot occur
                        // via the SDK). Nothing to write into; signal an empty error.
                        return -1i64;
                    };

                    // Read the three string parameters from the caller's memory.
                    let Ok(target) =
                        read_string_from_memory(&memory, &caller, plugin_ptr, plugin_len)
                    else {
                        return encode_result(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            Err(format!(
                                "{ERR_TARGET_ERRORED}: invalid plugin-name parameter"
                            )),
                        );
                    };
                    let Ok(function) = read_string_from_memory(&memory, &caller, fn_ptr, fn_len)
                    else {
                        return encode_result(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            Err(format!(
                                "{ERR_TARGET_ERRORED}: invalid function-name parameter"
                            )),
                        );
                    };
                    let Ok(payload) =
                        read_string_from_memory(&memory, &caller, payload_ptr, payload_len)
                    else {
                        return encode_result(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            Err(format!("{ERR_TARGET_ERRORED}: invalid payload parameter")),
                        );
                    };

                    // The runtime handle (for target lookup + dispatch) and a clone of
                    // this request's state (for the target Store) come from the caller's
                    // PluginState. Cloning out ends the immutable borrow before we write.
                    let runtime = caller
                        .data()
                        .request
                        .services()
                        .and_then(|s| s.plugin_runtime().cloned());
                    let state = caller.data().request.clone();

                    let result = match runtime {
                        Some(rt) => {
                            do_invoke(rt.as_ref(), &state, &target, &function, &payload).await
                        }
                        // No runtime attached (serviceless/test context): no target is
                        // resolvable.
                        None => Err(format!("{ERR_TARGET_NOT_FOUND}: {target}")),
                    };

                    encode_result(&memory, &mut caller, out_ptr, out_max_len, result)
                })
            },
        )
        .into_anyhow()?;

    // plugin-exists(name_ptr, name_len) -> i32 (1 = invocable, 0 = not)
    linker
        .func_wrap(
            "trovato:kernel/plugin-api",
            "plugin-exists",
            |mut caller: Caller<'_, PluginState>, name_ptr: i32, name_len: i32| -> i32 {
                let Some(Extern::Memory(memory)) = caller.get_export("memory") else {
                    return 0;
                };
                let Ok(name) = read_string_from_memory(&memory, &caller, name_ptr, name_len) else {
                    return 0;
                };
                let runtime = caller
                    .data()
                    .request
                    .services()
                    .and_then(|s| s.plugin_runtime().cloned());
                match runtime {
                    Some(rt) if plugin_is_invocable(rt.as_ref(), &name) => 1,
                    _ => 0,
                }
            },
        )
        .into_anyhow()?;

    Ok(())
}

/// Perform an `invoke` after applying the Model-C checks, then dispatch.
///
/// Returns `Ok(result_json)` on success, or `Err(message)` where `message` begins
/// with one of the frozen error prefixes. Pure of any WASM-memory concerns so it
/// is directly unit-testable against a real [`PluginRuntime`].
pub(crate) async fn do_invoke(
    runtime: &PluginRuntime,
    state: &RequestState,
    target: &str,
    function: &str,
    payload: &str,
) -> std::result::Result<String, String> {
    // Inbound payload cap — reject before any dispatch work.
    check_payload_size(payload.len())?;

    // Target must be installed + enabled (present in the runtime).
    let Some(plugin) = runtime.get_plugin(target) else {
        return Err(format!("{ERR_TARGET_NOT_FOUND}: {target}"));
    };

    // Callee consent gate (Model C): None ⇒ deny; function must be published.
    if !function_is_public(plugin.info.capabilities.as_ref(), function) {
        return Err(format!("{ERR_PERMISSION_DENIED}: {target}::{function}"));
    }

    // Recursion bound (Story 2.3): this dispatch would execute one level deeper
    // than the caller. Reject *before* any instantiation so an over-deep chain
    // stops cleanly with no partial commit and no runaway. Boundary: `>=` with
    // `depth = parent + 1` means the originating request runs at depth 0, up to
    // `MAX_INVOCATION_DEPTH - 1` nested invokes succeed, and the next is rejected
    // at `depth 8 >= 8`. The message matches the frozen template exactly.
    let depth = state.invocation_depth + 1;
    if depth >= MAX_INVOCATION_DEPTH {
        return Err(format!(
            "{ERR_RECURSION_LIMIT}: depth {depth} >= {MAX_INVOCATION_DEPTH}"
        ));
    }

    // Dispatch through the shared Store/export/memory primitive, resolving the
    // requested function as an arbitrary named export. The target runs with a
    // clone of this request's state carrying the incremented depth, so a target
    // that itself invokes is bounded by the same counter (no shared mutable state
    // — each frame owns its clone).
    let mut child = state.clone();
    child.invocation_depth = depth;
    let output = match instantiate_and_call_export(
        runtime,
        plugin.as_ref(),
        function,
        payload,
        child,
        INVOKE_EPOCH_DEADLINE,
    )
    .await
    {
        Ok(o) => o,
        Err(ExportCallError::ExportMissing) => {
            return Err(format!("{ERR_FUNCTION_NOT_EXPORTED}: {target}::{function}"));
        }
        Err(ExportCallError::Failed(e)) => {
            return Err(format!("{ERR_TARGET_ERRORED}: {e:#}"));
        }
    };

    // Outbound result cap.
    check_payload_size(output.len())?;
    Ok(output)
}

/// Whether `name` is an invocable plugin: installed + enabled (present in the
/// runtime) **and** exposing ≥1 public function (design §3.5 — invocability-aware,
/// not a plain installed-check).
pub(crate) fn plugin_is_invocable(runtime: &PluginRuntime, name: &str) -> bool {
    match runtime.get_plugin(name) {
        Some(plugin) => plugin
            .info
            .capabilities
            .as_ref()
            .is_some_and(|c| !c.public_functions.is_empty()),
        None => false,
    }
}

/// Enforce the frozen 1 MiB cap on a payload/result string length.
fn check_payload_size(len: usize) -> std::result::Result<(), String> {
    if len > MAX_PAYLOAD_BYTES {
        Err(format!(
            "{ERR_PAYLOAD_TOO_LARGE}: {len} > {MAX_PAYLOAD_BYTES}"
        ))
    } else {
        Ok(())
    }
}

/// Model C callee consent check: `None ⇒ deny`; the function must appear in the
/// target's `public_functions` allowlist.
fn function_is_public(caps: Option<&PluginCapabilities>, function: &str) -> bool {
    caps.is_some_and(|c| c.public_functions.iter().any(|f| f == function))
}

/// Encode a `Result<String, String>` into the caller's output buffer using the
/// length-tagged `i64` ABI (see the module ABI note).
fn encode_result(
    memory: &Memory,
    caller: &mut Caller<'_, PluginState>,
    out_ptr: i32,
    out_max_len: i32,
    result: std::result::Result<String, String>,
) -> i64 {
    match result {
        Ok(s) => match write_string_to_memory(memory, caller, out_ptr, out_max_len, &s) {
            // Fully written: success, return the byte length (>= 0).
            Ok(written) if written as usize == s.len() => written as i64,
            // Truncated: the buffer was smaller than the result. With the SDK
            // sizing its buffer to MAX_PAYLOAD_BYTES and the host capping results
            // at the same bound, this should not happen — surface it as an error
            // rather than silently returning a truncated string.
            Ok(_) => encode_err(
                memory,
                caller,
                out_ptr,
                out_max_len,
                &format!(
                    "{ERR_PAYLOAD_TOO_LARGE}: {} > {}",
                    s.len(),
                    MAX_PAYLOAD_BYTES
                ),
            ),
            Err(_) => encode_err(
                memory,
                caller,
                out_ptr,
                out_max_len,
                &format!("{ERR_TARGET_ERRORED}: failed to write invocation result"),
            ),
        },
        Err(msg) => encode_err(memory, caller, out_ptr, out_max_len, &msg),
    }
}

/// Write an error string to the output buffer and return its negative length tag
/// (`-(written) - 1`), so the SDK decodes it as `Err(out[0..written])`.
fn encode_err(
    memory: &Memory,
    caller: &mut Caller<'_, PluginState>,
    out_ptr: i32,
    out_max_len: i32,
    msg: &str,
) -> i64 {
    let written = write_string_to_memory(memory, caller, out_ptr, out_max_len, msg).unwrap_or(0);
    -(written as i64) - 1
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::plugin::{PluginConfig, PluginRuntime};
    use crate::tap::{RequestServices, RequestState, UserContext};
    use std::sync::Arc;

    /// Build a runtime with a single WAT-fixture plugin loaded under `name`.
    ///
    /// `capabilities`: `None` writes no `public_functions` (so the callee exposes
    /// no invocable surface); `Some(fns)` writes `public_functions = fns` (an
    /// empty slice declares the table but publishes nothing). The `.wasm` file is
    /// WAT text — `Module::new` parses it via the `wat` feature, exactly as the
    /// existing plugin integration tests do.
    ///
    /// Under WASM-1's per-plugin linker (deny-unless-declared), a fixture whose
    /// module imports `trovato:kernel/plugin-api` (e.g. `RECURSE_WAT`) must also
    /// declare `host_interfaces = ["plugin-api"]` or the load-time import
    /// pre-check rejects it. This helper auto-declares that interface when the WAT
    /// imports it, keeping the callee-gate (`public_functions`) tests focused on
    /// the callee axis.
    fn runtime_with_fixture(name: &str, wat: &str, capabilities: Option<&[&str]>) -> PluginRuntime {
        let dir = std::env::temp_dir().join(format!("trovato_invoke_fixture_{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create fixture dir");

        // WASM-1 caller gate: declare plugin-api iff the module imports it.
        let host_ifaces_line = if wat.contains("trovato:kernel/plugin-api") {
            "host_interfaces = [\"plugin-api\"]\n".to_string()
        } else {
            String::new()
        };
        let public_fns_line = match capabilities {
            None => String::new(),
            Some(fns) => {
                let list = fns
                    .iter()
                    .map(|f| format!("\"{f}\""))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("public_functions = [{list}]\n")
            }
        };
        let caps_section = if host_ifaces_line.is_empty() && public_fns_line.is_empty() {
            String::new()
        } else {
            format!("\n[capabilities]\n{host_ifaces_line}{public_fns_line}")
        };
        let info = format!(
            "name = \"{name}\"\ndescription = \"invoke fixture\"\nversion = \"1.0.0\"\n{caps_section}"
        );
        std::fs::write(dir.join(format!("{name}.info.toml")), info).expect("write info");
        std::fs::write(dir.join(format!("{name}.wasm")), wat).expect("write wasm");

        let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
        runtime.load_plugin(&dir).expect("load fixture plugin");
        runtime
    }

    /// A fixture exporting `echo` that returns the constant JSON `{"ok":true}`
    /// (11 bytes at offset 2048) via the memory protocol.
    const ECHO_WAT: &str = r#"
        (module
          (memory (export "memory") 2)
          (data (i32.const 2048) "{\"ok\":true}")
          (func (export "echo") (param i32 i32) (result i64)
            (i64.or
              (i64.shl (i64.const 2048) (i64.const 32))
              (i64.const 11))))
    "#;

    /// A fixture that invokes *itself* (`selfrec::recurse`) through the real host
    /// `invoke` import, then returns whatever that call produced as its own result.
    ///
    /// It imports `plugin-api::invoke`, targets its own published `recurse` export
    /// with a `{}` payload, and re-returns the host's output buffer via the memory
    /// protocol. The host's length-tagged `i64` is `>= 0` on success (the call
    /// returned a result string) and `< 0` on failure (an error string of length
    /// `(-r) - 1`); the guest normalizes both to the byte length actually written
    /// at offset 1024 and returns `(1024 << 32) | len`, so the *innermost*
    /// `recursion-limit-exceeded` string propagates up the chain unchanged as each
    /// frame's `Ok` result. This drives a genuine N-deep chain across real `Store`
    /// boundaries and proves it terminates at the cap rather than running away.
    const RECURSE_WAT: &str = r#"
        (module
          (import "trovato:kernel/plugin-api" "invoke"
            (func $invoke (param i32 i32 i32 i32 i32 i32 i32 i32) (result i64)))
          (memory (export "memory") 2)
          (data (i32.const 100) "selfrec")
          (data (i32.const 200) "recurse")
          (data (i32.const 300) "{}")
          (func (export "recurse") (param i32 i32) (result i64)
            (local $r i64)
            (local $len i64)
            (local.set $r
              (call $invoke
                (i32.const 100) (i32.const 7)   ;; target  = "selfrec"
                (i32.const 200) (i32.const 7)   ;; function = "recurse"
                (i32.const 300) (i32.const 2)   ;; payload  = "{}"
                (i32.const 1024) (i32.const 256))) ;; out buffer
            ;; len = (r < 0) ? (-r - 1) : r
            (local.set $len
              (select
                (i64.sub (i64.sub (i64.const 0) (local.get $r)) (i64.const 1))
                (local.get $r)
                (i64.lt_s (local.get $r) (i64.const 0))))
            (i64.or
              (i64.shl (i64.const 1024) (i64.const 32))
              (local.get $len))))
    "#;

    fn anon_state() -> RequestState {
        RequestState::without_services(UserContext::anonymous())
    }

    /// Build a `RequestState` carrying a (lazy, never-connected) services bundle
    /// with `runtime` attached, so the guest's `invoke` host call can resolve a
    /// target. The lazy pool is never queried by the recursion fixtures, so no
    /// Postgres is required — this stays a unit test.
    fn state_with_runtime(runtime: Arc<PluginRuntime>) -> RequestState {
        let db = sqlx::postgres::PgPool::connect_lazy("postgres://localhost/trovato")
            .expect("lazy pool");
        let services = RequestServices::for_background(db, None, None, reqwest::Client::new())
            .with_plugin_runtime(runtime);
        RequestState::new(UserContext::anonymous(), services)
    }

    // --- pure helpers ----------------------------------------------------

    #[test]
    fn payload_size_cap_boundary() {
        assert!(check_payload_size(0).is_ok());
        assert!(check_payload_size(MAX_PAYLOAD_BYTES).is_ok());
        let err = check_payload_size(MAX_PAYLOAD_BYTES + 1).unwrap_err();
        assert!(err.starts_with(ERR_PAYLOAD_TOO_LARGE));
        assert!(err.contains(&format!("{} > {MAX_PAYLOAD_BYTES}", MAX_PAYLOAD_BYTES + 1)));
    }

    #[test]
    fn function_is_public_allow_deny_and_none() {
        let caps = PluginCapabilities {
            public_functions: vec!["alpha".to_string(), "beta".to_string()],
            ..Default::default()
        };
        assert!(function_is_public(Some(&caps), "alpha"));
        assert!(function_is_public(Some(&caps), "beta"));
        // Declared, but this function is not published ⇒ deny.
        assert!(!function_is_public(Some(&caps), "gamma"));
        // capabilities: None ⇒ deny.
        assert!(!function_is_public(None, "alpha"));
        // Empty list ⇒ deny.
        let empty = PluginCapabilities::default();
        assert!(!function_is_public(Some(&empty), "alpha"));
    }

    #[test]
    fn frozen_error_vocabulary_prefixes() {
        // The set of prefixes is the frozen contract (design §3.3); assert the
        // exact six kebab strings so a typo or rename is caught.
        assert_eq!(ERR_TARGET_NOT_FOUND, "target-not-found");
        assert_eq!(ERR_PERMISSION_DENIED, "permission-denied");
        assert_eq!(ERR_FUNCTION_NOT_EXPORTED, "function-not-exported");
        assert_eq!(ERR_TARGET_ERRORED, "target-errored");
        assert_eq!(ERR_PAYLOAD_TOO_LARGE, "payload-too-large");
        // Reserved for Story 2.3 (no emitter in 2.2).
        assert_eq!(ERR_RECURSION_LIMIT, "recursion-limit-exceeded");
    }

    // --- do_invoke through the real dispatch path ------------------------

    #[tokio::test]
    async fn invoke_happy_path_returns_result() {
        let runtime = runtime_with_fixture("inv_echo", ECHO_WAT, Some(&["echo"]));
        let out = do_invoke(&runtime, &anon_state(), "inv_echo", "echo", "{}")
            .await
            .expect("invoke should succeed");
        assert_eq!(out, r#"{"ok":true}"#);
    }

    #[tokio::test]
    async fn invoke_target_not_found() {
        // Empty runtime, no plugins loaded.
        let runtime = PluginRuntime::new(&PluginConfig::default()).unwrap();
        let err = do_invoke(&runtime, &anon_state(), "ghost", "echo", "{}")
            .await
            .unwrap_err();
        assert_eq!(err, format!("{ERR_TARGET_NOT_FOUND}: ghost"));
    }

    #[tokio::test]
    async fn invoke_permission_denied_for_unpublished_function() {
        let runtime = runtime_with_fixture("inv_perm", ECHO_WAT, Some(&["echo"]));
        let err = do_invoke(&runtime, &anon_state(), "inv_perm", "secret", "{}")
            .await
            .unwrap_err();
        assert_eq!(err, format!("{ERR_PERMISSION_DENIED}: inv_perm::secret"));
    }

    #[tokio::test]
    async fn invoke_none_capabilities_denies() {
        // No [capabilities] table ⇒ capabilities None ⇒ deny even for an exported
        // function.
        let runtime = runtime_with_fixture("inv_none", ECHO_WAT, None);
        let err = do_invoke(&runtime, &anon_state(), "inv_none", "echo", "{}")
            .await
            .unwrap_err();
        assert_eq!(err, format!("{ERR_PERMISSION_DENIED}: inv_none::echo"));
    }

    #[tokio::test]
    async fn invoke_function_not_exported() {
        // Published in the manifest, but the WASM does not export it.
        let wat = r#"(module (memory (export "memory") 1))"#;
        let runtime = runtime_with_fixture("inv_missing", wat, Some(&["phantom"]));
        let err = do_invoke(&runtime, &anon_state(), "inv_missing", "phantom", "{}")
            .await
            .unwrap_err();
        assert_eq!(
            err,
            format!("{ERR_FUNCTION_NOT_EXPORTED}: inv_missing::phantom")
        );
    }

    #[tokio::test]
    async fn invoke_target_errored_on_trap() {
        let wat = r#"
            (module
              (memory (export "memory") 1)
              (func (export "boom") (param i32 i32) (result i64) unreachable))
        "#;
        let runtime = runtime_with_fixture("inv_boom", wat, Some(&["boom"]));
        let err = do_invoke(&runtime, &anon_state(), "inv_boom", "boom", "{}")
            .await
            .unwrap_err();
        assert!(
            err.starts_with(ERR_TARGET_ERRORED),
            "expected target-errored, got: {err}"
        );
    }

    #[tokio::test]
    async fn invoke_inbound_payload_too_large() {
        // Rejected before any target lookup, so an empty runtime suffices.
        let runtime = PluginRuntime::new(&PluginConfig::default()).unwrap();
        let payload = "a".repeat(MAX_PAYLOAD_BYTES + 1);
        let err = do_invoke(&runtime, &anon_state(), "any", "any", &payload)
            .await
            .unwrap_err();
        assert!(err.starts_with(ERR_PAYLOAD_TOO_LARGE), "got: {err}");
    }

    #[tokio::test]
    async fn invoke_outbound_result_too_large() {
        // `big` returns a result of MAX_PAYLOAD_BYTES + 1 bytes from a 17-page
        // memory; the outbound cap rejects it.
        let wat = r#"
            (module
              (memory (export "memory") 17)
              (func (export "big") (param i32 i32) (result i64)
                (i64.or
                  (i64.shl (i64.const 0) (i64.const 32))
                  (i64.const 1048577))))
        "#;
        let runtime = runtime_with_fixture("inv_big", wat, Some(&["big"]));
        let err = do_invoke(&runtime, &anon_state(), "inv_big", "big", "{}")
            .await
            .unwrap_err();
        assert!(err.starts_with(ERR_PAYLOAD_TOO_LARGE), "got: {err}");
    }

    // --- recursion bound (Story 2.3) -------------------------------------

    #[tokio::test]
    async fn invoke_rejects_at_recursion_cap() {
        // A state already one short of the cap: this dispatch would run at
        // `MAX_INVOCATION_DEPTH` and must be rejected *before* dispatch — even
        // though `echo` is published and would otherwise succeed.
        let runtime = runtime_with_fixture("inv_depth", ECHO_WAT, Some(&["echo"]));
        let mut state = anon_state();
        state.invocation_depth = MAX_INVOCATION_DEPTH - 1;
        let err = do_invoke(&runtime, &state, "inv_depth", "echo", "{}")
            .await
            .unwrap_err();
        // Exact frozen message template: `recursion-limit-exceeded: depth 8 >= 8`.
        assert_eq!(
            err,
            format!(
                "{ERR_RECURSION_LIMIT}: depth {MAX_INVOCATION_DEPTH} >= {MAX_INVOCATION_DEPTH}"
            )
        );
    }

    #[tokio::test]
    async fn invoke_succeeds_one_below_recursion_cap() {
        // Pins the other side of the off-by-one: a dispatch that runs at exactly
        // `MAX_INVOCATION_DEPTH - 1` (the deepest *allowed* level) still succeeds.
        let runtime = runtime_with_fixture("inv_depth_ok", ECHO_WAT, Some(&["echo"]));
        let mut state = anon_state();
        state.invocation_depth = MAX_INVOCATION_DEPTH - 2;
        let out = do_invoke(&runtime, &state, "inv_depth_ok", "echo", "{}")
            .await
            .expect("dispatch one below the cap should succeed");
        assert_eq!(out, r#"{"ok":true}"#);
    }

    #[tokio::test]
    async fn invoke_recursion_chain_stops_at_cap() {
        // Genuine self-invocation through the real host `invoke` import: the chain
        // recurses across real Store boundaries until the depth check rejects the
        // dispatch that would reach the cap. It must terminate deterministically
        // (no runaway / stack overflow) and surface the frozen message — proving
        // the depth actually propagates parent→child across the dispatch boundary.
        let runtime = Arc::new(runtime_with_fixture(
            "selfrec",
            RECURSE_WAT,
            Some(&["recurse"]),
        ));
        let state = state_with_runtime(Arc::clone(&runtime));
        let out = do_invoke(runtime.as_ref(), &state, "selfrec", "recurse", "{}")
            .await
            .expect("chain should terminate cleanly and return the propagated result");
        // The innermost rejection (`depth 8 >= 8`) propagates up unchanged.
        assert_eq!(
            out,
            format!(
                "{ERR_RECURSION_LIMIT}: depth {MAX_INVOCATION_DEPTH} >= {MAX_INVOCATION_DEPTH}"
            )
        );
    }

    // --- plugin_is_invocable ---------------------------------------------

    #[test]
    fn plugin_exists_true_when_publishes_functions() {
        let runtime = runtime_with_fixture("pe_yes", ECHO_WAT, Some(&["echo"]));
        assert!(plugin_is_invocable(&runtime, "pe_yes"));
    }

    #[test]
    fn plugin_exists_false_when_installed_but_no_public_functions() {
        // Declares [capabilities] but publishes nothing ⇒ not invocable.
        let runtime = runtime_with_fixture("pe_empty", ECHO_WAT, Some(&[]));
        assert!(!plugin_is_invocable(&runtime, "pe_empty"));
    }

    #[test]
    fn plugin_exists_false_when_no_capabilities_table() {
        let runtime = runtime_with_fixture("pe_none", ECHO_WAT, None);
        assert!(!plugin_is_invocable(&runtime, "pe_none"));
    }

    #[test]
    fn plugin_exists_false_when_absent() {
        let runtime = PluginRuntime::new(&PluginConfig::default()).unwrap();
        assert!(!plugin_is_invocable(&runtime, "ghost"));
    }
}
