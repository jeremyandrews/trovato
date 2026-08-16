#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Story 2.4 — end-to-end plugin-to-plugin invocation + sandbox integration.
//!
//! Everything in Epic 2 until this story was proven with in-test WAT fixtures.
//! This suite re-proves the **frozen** FR-4a surface through plugins built from
//! Rust source with the real `trovato-plugin-sdk` and the real `wasm32-wasip1`
//! toolchain — the exact path a plugin author uses — driven through the real
//! kernel (`PluginRuntime` + `TapDispatcher` + the registered host `invoke`).
//!
//! # How the surface is driven
//!
//! `do_invoke` is `pub(crate)`, so an integration test cannot call it. Instead the
//! `test_e2e_caller` fixture declares `host_interfaces = ["plugin-api"]` and
//! implements a `tap_cron` **invocation driver**: the test dispatches it a JSON
//! command via the public `TapDispatcher`, and the driver calls the real SDK
//! `invoke()` / `plugin_exists()` and returns the outcome as JSON. So every
//! assertion travels the whole path: caller WASM → SDK `invoke()` → host `invoke`
//! → callee WASM. Error assertions are on the **exact** frozen strings (about to
//! freeze at FR-4), not substrings.
//!
//! # Fixtures (built by CI before the test job; `.wasm` are gitignored)
//!
//! - `test_e2e_caller` — declares `plugin-api`; the driver.
//! - `test_e2e_callee` — publishes `echo`/`big_out`/`recurse`/`read_undeclared`/
//!   `greedy_mem`/`not_exported`; exports an *unpublished* `secret`; owns one
//!   migration table so its WASM-2 db policy has a real allowlist.
//! - `test_e2e_bystander` — imports `plugin-api` only; ships no manifest, loaded
//!   here under a manifest that omits `plugin-api` to exercise the caller gate.
//! - `test_e2e_nocaps` — no `[capabilities]` table; exports `ping`.
//!
//! # No infrastructure required
//!
//! None of these tests needs Postgres/Redis: the invoke checks are pre-dispatch or
//! pure-WASM, and the one db path (`read_undeclared`) is rejected by the WASM-2
//! raw-SQL gate *before* any pool access, so a lazy, never-connected pool suffices.
//! The tests therefore always run (no self-skip), and CI guarantees the fixtures
//! are built.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use trovato_kernel::plugin::limits::ResourceLimits;
use trovato_kernel::plugin::{DbPolicy, PluginConfig, PluginInfo, PluginRuntime};
use trovato_kernel::tap::{RequestServices, RequestState, TapDispatcher, TapRegistry, UserContext};

/// The frozen 1 MiB payload/result cap (kernel `MAX_PAYLOAD_BYTES`), mirrored so
/// the payload-cap tests can construct the boundary without a kernel export.
const MAX_PAYLOAD_BYTES: usize = 1_048_576;

/// Repo `plugins/` directory (two levels up from this crate).
fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

/// Build a runtime and load the named e2e fixtures from `plugins/<name>/`.
///
/// Panics with a build hint if a fixture `.wasm` is missing — CI builds them
/// before the test job, and locally `cargo build -p <name> --target wasm32-wasip1
/// --release` + copy into `plugins/<name>/` is the same step.
fn runtime_loading(config: &PluginConfig, names: &[&str]) -> Arc<PluginRuntime> {
    let mut runtime = PluginRuntime::new(config).expect("create runtime");
    for name in names {
        let dir = plugins_dir().join(name);
        runtime.load_plugin(&dir).unwrap_or_else(|e| {
            panic!(
                "failed to load fixture '{name}': {e:#}\n\
                 build it first: cargo build -p {name} --target wasm32-wasip1 --release \
                 && cp target/wasm32-wasip1/release/{name}.wasm plugins/{name}/"
            )
        });
    }
    Arc::new(runtime)
}

/// The default-limits runtime shared by most tests: caller + callee + nocaps.
fn shared_runtime() -> Arc<PluginRuntime> {
    runtime_loading(
        &PluginConfig::default(),
        &["test_e2e_caller", "test_e2e_callee", "test_e2e_nocaps"],
    )
}

/// A fresh request state carrying the runtime (so the host `invoke`/`plugin-exists`
/// can resolve targets) and a lazy, never-connected pool (no live Postgres).
fn request_state(runtime: &Arc<PluginRuntime>) -> RequestState {
    let db =
        sqlx::postgres::PgPool::connect_lazy("postgres://localhost/trovato").expect("lazy pool");
    let services = RequestServices::for_background(db, None, None, reqwest::Client::new())
        .with_plugin_runtime(Arc::clone(runtime));
    RequestState::new(UserContext::anonymous(), services)
}

/// Dispatch a command to the caller's `tap_cron` driver and return its parsed JSON
/// outcome (`{"ok"|"err": ...}` for an invoke, `{"exists": bool}` for `@exists`).
async fn drive(runtime: &Arc<PluginRuntime>, command: Value) -> Value {
    let registry = Arc::new(TapRegistry::from_plugins(runtime));
    let dispatcher = TapDispatcher::new(Arc::clone(runtime), registry);
    let input = serde_json::to_string(&command).expect("serialize command");
    let result = dispatcher
        .dispatch_to_plugin(
            "tap_cron",
            &input,
            "test_e2e_caller",
            request_state(runtime),
        )
        .await
        .expect("caller tap_cron produced a result");
    serde_json::from_str(&result.output).expect("driver returned JSON")
}

/// Convenience: drive an `invoke(target, function, payload)` and return the parsed
/// outcome.
async fn invoke(
    runtime: &Arc<PluginRuntime>,
    target: &str,
    function: &str,
    payload: &str,
) -> Value {
    drive(
        runtime,
        json!({ "target": target, "function": function, "payload": payload }),
    )
    .await
}

/// Extract the frozen error string from an `{"err": ...}` outcome.
fn err_of(outcome: &Value) -> &str {
    outcome
        .get("err")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected an err outcome, got: {outcome}"))
}

/// Extract the `{"ok": ...}` result string from a successful outcome.
fn ok_of(outcome: &Value) -> &str {
    outcome
        .get("ok")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected an ok outcome, got: {outcome}"))
}

// =============================================================================
// 1. Happy path — payload round-trips intact through a real invoke.
// =============================================================================

#[tokio::test]
async fn happy_path_roundtrips_json_payload() {
    let runtime = shared_runtime();
    let payload = json!({
        "n": 42,
        "s": "trovato",
        "nested": { "list": [1, 2, 3], "flag": true, "null": null }
    });

    let outcome = invoke(&runtime, "test_e2e_callee", "echo", &payload.to_string()).await;
    let echoed: Value = serde_json::from_str(ok_of(&outcome)).expect("echo result is JSON");

    assert_eq!(
        echoed, payload,
        "echo must return the caller's payload unchanged across the invoke boundary"
    );
}

// =============================================================================
// 2. permission-denied — invoking an exported-but-unpublished function.
// =============================================================================

#[tokio::test]
async fn permission_denied_for_unpublished_function() {
    let runtime = shared_runtime();
    // `secret` is a real WASM export but absent from `public_functions`.
    let outcome = invoke(&runtime, "test_e2e_callee", "secret", "{}").await;
    assert_eq!(
        err_of(&outcome),
        "permission-denied: test_e2e_callee::secret"
    );
}

// =============================================================================
// 3. target-not-found and function-not-exported — exact strings.
// =============================================================================

#[tokio::test]
async fn target_not_found_for_unknown_plugin() {
    let runtime = shared_runtime();
    let outcome = invoke(&runtime, "test_e2e_ghost", "echo", "{}").await;
    assert_eq!(err_of(&outcome), "target-not-found: test_e2e_ghost");
}

#[tokio::test]
async fn function_not_exported_for_published_but_missing() {
    let runtime = shared_runtime();
    // `not_exported` IS published (passes the consent gate) but the WASM has no
    // such export — so the dispatch layer reports it distinctly.
    let outcome = invoke(&runtime, "test_e2e_callee", "not_exported", "{}").await;
    assert_eq!(
        err_of(&outcome),
        "function-not-exported: test_e2e_callee::not_exported"
    );
}

// =============================================================================
// 4. capabilities: None denies everything — even an exported function.
// =============================================================================

#[tokio::test]
async fn capabilities_none_denies_exported_function() {
    let runtime = shared_runtime();
    // test_e2e_nocaps ships no [capabilities] table, so it publishes nothing —
    // invoking its real `ping` export is denied by the callee-consent gate.
    let outcome = invoke(&runtime, "test_e2e_nocaps", "ping", "{}").await;
    assert_eq!(err_of(&outcome), "permission-denied: test_e2e_nocaps::ping");
}

// =============================================================================
// 5. Caller gate — a plugin that imports `invoke` without declaring `plugin-api`
//    fails the WASM-1 load-time import pre-check (a real SDK binary).
// =============================================================================

#[tokio::test]
async fn caller_gate_rejects_undeclared_plugin_api_at_load() {
    // The bystander's compiled module imports trovato:kernel/plugin-api and
    // nothing else. Load it under a manifest that does NOT declare plugin-api.
    let wasm = std::fs::read(plugins_dir().join("test_e2e_bystander/test_e2e_bystander.wasm"))
        .expect(
            "bystander .wasm missing; build: cargo build -p test_e2e_bystander \
             --target wasm32-wasip1 --release && cp into plugins/test_e2e_bystander/",
        );

    let dir = std::env::temp_dir().join("trovato_e2e_bystander_gate");
    std::fs::create_dir_all(&dir).unwrap();
    // No [capabilities] table => capabilities None => plugin-api not declared.
    std::fs::write(
        dir.join("test_e2e_bystander.info.toml"),
        "name = \"test_e2e_bystander\"\ndescription = \"caller-gate probe\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("test_e2e_bystander.wasm"), &wasm).unwrap();

    let mut runtime = PluginRuntime::new(&PluginConfig::default()).unwrap();
    let err = runtime
        .load_plugin(&dir)
        .expect_err("a plugin importing plugin-api without declaring it must fail to load")
        .to_string();
    std::fs::remove_dir_all(&dir).ok();

    // The WASM-1 load-time pre-check turns the raw mid-request "unknown import"
    // into a declarative startup error naming the plugin, the interface, and the fix.
    assert!(
        err.contains("imports host interface 'plugin-api'")
            && err.contains("does not declare")
            && err.contains("test_e2e_bystander"),
        "expected the declarative plugin-api caller-gate error, got: {err}"
    );
}

// =============================================================================
// 6. Recursion — a self-invoking chain terminates at exactly the frozen boundary.
// =============================================================================

#[tokio::test]
async fn recursion_chain_stops_at_frozen_boundary() {
    let runtime = shared_runtime();
    // callee::recurse self-invokes and returns the inner result verbatim, so the
    // innermost rejection propagates to the top unchanged. The origin runs at
    // depth 0; 7 nested invokes succeed and the 8th is rejected at depth 8 >= 8.
    let outcome = invoke(&runtime, "test_e2e_callee", "recurse", "{}").await;
    assert_eq!(ok_of(&outcome), "recursion-limit-exceeded: depth 8 >= 8");
}

// =============================================================================
// 7. Payload cap — inbound and outbound rejections just over 1 MiB.
// =============================================================================

#[tokio::test]
async fn inbound_payload_over_cap_is_rejected() {
    let runtime = shared_runtime();
    let over = MAX_PAYLOAD_BYTES + 1;
    // The driver builds the oversize payload inside the caller WASM (payload_repeat)
    // so it never has to pass through the 64 KiB tap-input buffer.
    let outcome = drive(
        &runtime,
        json!({ "target": "test_e2e_callee", "function": "echo", "payload_repeat": over }),
    )
    .await;
    assert_eq!(err_of(&outcome), "payload-too-large: 1048577 > 1048576");
}

#[tokio::test]
async fn outbound_result_over_cap_is_rejected() {
    let runtime = shared_runtime();
    // callee::big_out returns MAX_PAYLOAD_BYTES + 1 bytes; the outbound check rejects it.
    let outcome = invoke(&runtime, "test_e2e_callee", "big_out", "{}").await;
    assert_eq!(err_of(&outcome), "payload-too-large: 1048577 > 1048576");
}

// =============================================================================
// 8. plugin-exists — true only for installed + enabled + ≥1 public function.
// =============================================================================

#[tokio::test]
async fn plugin_exists_semantics_through_real_wasm() {
    let runtime = shared_runtime();

    let exists = |target: &'static str| {
        let runtime = Arc::clone(&runtime);
        async move {
            drive(&runtime, json!({ "target": target, "function": "@exists" }))
                .await
                .get("exists")
                .and_then(Value::as_bool)
                .expect("exists outcome")
        }
    };

    // Installed + enabled + publishes functions.
    assert!(exists("test_e2e_callee").await, "callee is invocable");
    // Installed + enabled but publishes NO functions (caller has no public_functions).
    assert!(
        !exists("test_e2e_caller").await,
        "caller publishes nothing => not invocable"
    );
    // Installed but capabilities: None => no public surface.
    assert!(
        !exists("test_e2e_nocaps").await,
        "nocaps has no capabilities => not invocable"
    );
    // Not installed at all.
    assert!(
        !exists("test_e2e_ghost").await,
        "absent plugin => not invocable"
    );
}

// =============================================================================
// 9. Sandbox composition — WASM-2 (db) and WASM-4 (memory) through the invoke path.
// =============================================================================

/// The callee's raw-SQL read is rejected during a nested invoke. The SDK exposes
/// only the raw-SQL db path, and this callee does not declare `raw_sql`, so the
/// WASM-2 gate returns `ERR_RAW_SQL_NOT_DECLARED` (-17) before any pool access —
/// observed here as the numeric code the callee returns through the invoke result.
#[tokio::test]
async fn db_sandbox_blocks_undeclared_raw_sql_during_invoke() {
    let runtime = shared_runtime();
    let outcome = invoke(&runtime, "test_e2e_callee", "read_undeclared", "{}").await;
    let body: Value = serde_json::from_str(ok_of(&outcome)).expect("db result JSON");
    assert_eq!(
        body.get("db_code").and_then(Value::as_i64),
        Some(i64::from(
            trovato_sdk::host_errors::ERR_RAW_SQL_NOT_DECLARED
        )),
        "raw SQL without a declared raw_sql capability must be gated (-17): {body}"
    );
}

/// Pins the exact frozen WASM-2 error *strings* for the real callee fixture at the
/// policy layer — where, by design, they are produced (the `db` host functions
/// cross the ABI as a numeric code; the string is the host-side rendering, see
/// `db_policy.rs`). Derives the callee's real effective policy from its shipped
/// manifest + migration.
#[test]
fn db_sandbox_frozen_strings_for_callee_policy() {
    let dir = plugins_dir().join("test_e2e_callee");
    let info = PluginInfo::parse(&dir.join("test_e2e_callee.info.toml")).expect("parse manifest");
    let policy = DbPolicy::derive(&info, &dir);

    // Migration-owned table is allowed.
    assert!(
        policy.check_table("callee_owned").is_ok(),
        "the callee's own migration table must be in the allowlist"
    );
    // Undeclared table -> exact frozen message.
    assert_eq!(
        policy.check_table("undeclared_table").unwrap_err(),
        "table-not-declared: undeclared_table (plugin test_e2e_callee)"
    );
    // Raw SQL without the capability -> exact frozen message.
    assert_eq!(
        policy.check_raw_sql().unwrap_err(),
        "raw-sql-not-declared: test_e2e_callee"
    );
}

/// A memory-greedy callee is stopped by the WASM-4 per-`Store` limiter and the
/// trap surfaces to the caller as `target-errored` (with the attributed
/// `memory-limit-exceeded` detail), not as a kernel failure. Uses a small memory
/// cap rather than shipping a fixture that actually allocates 64 MiB.
#[tokio::test]
async fn greedy_callee_stopped_by_limiter_surfaces_target_errored() {
    let config = PluginConfig {
        limits: ResourceLimits {
            max_memory_bytes: 16 * 1024 * 1024,
            ..ResourceLimits::default()
        },
        ..PluginConfig::default()
    };
    // Small-cap runtime with just the caller + callee.
    let runtime = runtime_loading(&config, &["test_e2e_caller", "test_e2e_callee"]);

    let outcome = invoke(&runtime, "test_e2e_callee", "greedy_mem", "{}").await;
    let err = err_of(&outcome);
    assert!(
        err.starts_with("target-errored"),
        "a limiter trap must surface to the caller as target-errored, got: {err}"
    );
    assert!(
        err.contains("memory-limit-exceeded"),
        "the target-errored detail must carry the attributed limiter message, got: {err}"
    );
}
