#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Integration tests for the plugin system.
//!
//! ## Prerequisites
//!
//! Build the blog plugin to WASM before running:
//! ```bash
//! cargo build -p blog --target wasm32-wasip1 --release
//! cp target/wasm32-wasip1/release/blog.wasm plugins/blog/
//! ```
//!
//! ## Running Tests
//!
//! ```bash
//! cargo test --test plugin_test
//! ```
//!
//! ## Test Coverage
//!
//! - Runtime creation with default/custom config
//! - Plugin loading from directory
//! - Single plugin loading
//! - Plugin metadata parsing
//! - Error handling (missing WASM, invalid TOML, unknown taps)
//! - Graceful handling of missing plugins directory

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

use trovato_kernel::host;
use trovato_kernel::menu::MenuRegistry;
use trovato_kernel::plugin::limits::ResourceLimits;
use trovato_kernel::plugin::{
    DbPolicy, KNOWN_HOST_INTERFACES, KNOWN_TAPS, PluginConfig, PluginInfo, PluginRuntime,
    PluginState, resolve_load_order,
};
use trovato_kernel::tap::{RequestServices, RequestState, TapDispatcher, TapRegistry, UserContext};
use uuid::Uuid;
use wasmtime::{Engine, Linker, Module, Store};

/// Test that PluginRuntime can be created with default config.
#[test]
fn create_runtime_default_config() {
    let runtime = PluginRuntime::new(&PluginConfig::default());
    assert!(
        runtime.is_ok(),
        "Failed to create runtime: {:?}",
        runtime.err()
    );
}

/// Test that PluginRuntime can be created with custom config.
#[test]
fn create_runtime_custom_config() {
    let config = PluginConfig {
        max_instances: 100,
        max_memory_pages: 256,
        ..PluginConfig::default()
    };
    let runtime = PluginRuntime::new(&config);
    assert!(
        runtime.is_ok(),
        "Failed to create runtime: {:?}",
        runtime.err()
    );
}

/// Test loading plugins from a directory.
#[tokio::test]
async fn load_plugins_from_directory() {
    let mut runtime =
        PluginRuntime::new(&PluginConfig::default()).expect("Failed to create runtime");

    let plugins_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins");

    // Should not fail even if some plugins have issues
    let result = runtime.load_all(&plugins_dir).await;
    assert!(result.is_ok(), "Failed to load plugins: {:?}", result.err());

    // Blog plugin should be loaded
    assert!(
        runtime.get_plugin("trovato_blog").is_some(),
        "trovato_blog plugin not loaded. Available: {:?}",
        runtime.plugins().keys().collect::<Vec<_>>()
    );
}

/// Test loading a single plugin.
#[test]
fn load_single_plugin() {
    let mut runtime =
        PluginRuntime::new(&PluginConfig::default()).expect("Failed to create runtime");

    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
        .join("trovato_blog");

    let result = runtime.load_plugin(&plugin_dir);
    assert!(
        result.is_ok(),
        "Failed to load blog plugin: {:?}",
        result.err()
    );

    let plugin = runtime
        .get_plugin("trovato_blog")
        .expect("Plugin not found");
    assert_eq!(plugin.info.name, "trovato_blog");
    // An in-tree plugin carries the one project version; asserting a literal
    // here would mean editing this test on every release.
    assert_eq!(plugin.info.version, env!("CARGO_PKG_VERSION"));
    assert!(
        plugin
            .info
            .taps
            .implements
            .contains(&"tap_item_info".to_string())
    );
    assert!(
        plugin
            .info
            .taps
            .implements
            .contains(&"tap_item_view".to_string())
    );
}

/// Test that missing WASM file produces clear error.
#[test]
fn missing_wasm_file_error() {
    let mut runtime =
        PluginRuntime::new(&PluginConfig::default()).expect("Failed to create runtime");

    // Create a temp directory with only .info.toml
    let temp_dir = std::env::temp_dir().join("trovato_test_plugin");
    std::fs::create_dir_all(&temp_dir).ok();
    std::fs::write(
        temp_dir.join("test.info.toml"),
        r#"
name = "test"
description = "Test plugin"
version = "1.0.0"
"#,
    )
    .expect("Failed to write test info");

    let result = runtime.load_plugin(&temp_dir);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("WASM file not found") || err.contains("wasm"),
        "Expected WASM error, got: {err}"
    );

    // Cleanup
    std::fs::remove_dir_all(&temp_dir).ok();
}

/// Test that invalid .info.toml produces clear error.
#[test]
fn invalid_info_toml_error() {
    let mut runtime =
        PluginRuntime::new(&PluginConfig::default()).expect("Failed to create runtime");

    let temp_dir = std::env::temp_dir().join("trovato_test_invalid");
    std::fs::create_dir_all(&temp_dir).ok();
    std::fs::write(
        temp_dir.join("invalid.info.toml"),
        "this is not valid toml {{{{",
    )
    .expect("Failed to write invalid info");

    let result = runtime.load_plugin(&temp_dir);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("parse") || err.contains("TOML"),
        "Expected parse error, got: {err}"
    );

    // Cleanup
    std::fs::remove_dir_all(&temp_dir).ok();
}

/// Test that unknown tap names are rejected.
#[test]
fn unknown_tap_rejected() {
    let mut runtime =
        PluginRuntime::new(&PluginConfig::default()).expect("Failed to create runtime");

    let temp_dir = std::env::temp_dir().join("trovato_test_unknown_tap");
    std::fs::create_dir_all(&temp_dir).ok();
    std::fs::write(
        temp_dir.join("bad.info.toml"),
        r#"
name = "bad"
description = "Bad plugin"
version = "1.0.0"

[taps]
implements = ["tap_unknown_function"]
"#,
    )
    .expect("Failed to write bad info");

    let result = runtime.load_plugin(&temp_dir);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("unknown tap"),
        "Expected unknown tap error, got: {err}"
    );

    // Cleanup
    std::fs::remove_dir_all(&temp_dir).ok();
}

/// Test loading from non-existent directory doesn't fail.
#[tokio::test]
async fn nonexistent_plugins_dir_ok() {
    let mut runtime =
        PluginRuntime::new(&PluginConfig::default()).expect("Failed to create runtime");

    let result = runtime
        .load_all(Path::new("/nonexistent/plugins/dir"))
        .await;
    assert!(result.is_ok(), "Should gracefully handle missing dir");
    assert_eq!(runtime.plugin_count(), 0);
}

/// Test plugin info metadata is correct.
#[test]
fn plugin_metadata_correct() {
    let mut runtime =
        PluginRuntime::new(&PluginConfig::default()).expect("Failed to create runtime");

    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
        .join("trovato_blog");

    runtime.load_plugin(&plugin_dir).expect("Failed to load");

    let plugin = runtime
        .get_plugin("trovato_blog")
        .expect("Plugin not found");

    // Check metadata
    assert_eq!(plugin.info.name, "trovato_blog");
    assert_eq!(
        plugin.info.description,
        "Provides a blog content type with tags"
    );
    // In-tree plugins carry the one project version; see load_single_plugin.
    assert_eq!(plugin.info.version, env!("CARGO_PKG_VERSION"));
    let expected_deps: Vec<String> = vec![];
    assert_eq!(plugin.info.dependencies, expected_deps);

    // Check taps
    assert_eq!(plugin.info.taps.weight, 0);
    assert!(
        plugin
            .info
            .taps
            .implements
            .contains(&"tap_item_info".to_string())
    );
    assert!(
        plugin
            .info
            .taps
            .implements
            .contains(&"tap_item_view".to_string())
    );
    assert!(
        plugin
            .info
            .taps
            .implements
            .contains(&"tap_item_access".to_string())
    );
    assert!(
        plugin
            .info
            .taps
            .implements
            .contains(&"tap_menu".to_string())
    );
    assert!(
        plugin
            .info
            .taps
            .implements
            .contains(&"tap_perm".to_string())
    );
}

// =============================================================================
// Tap Registry Integration Tests
// =============================================================================

/// Test creating a tap registry from plugins.
#[test]
fn tap_registry_indexes_taps() {
    let mut runtime =
        PluginRuntime::new(&PluginConfig::default()).expect("Failed to create runtime");

    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
        .join("trovato_blog");

    runtime
        .load_plugin(&plugin_dir)
        .expect("Failed to load blog");

    let registry = TapRegistry::from_plugins(&runtime);

    // Blog plugin registers 5 taps
    assert_eq!(registry.tap_count(), 5);
    assert!(registry.has_tap("tap_item_info"));
    assert!(registry.has_tap("tap_item_view"));
    assert!(registry.has_tap("tap_item_access"));
    assert!(registry.has_tap("tap_menu"));
    assert!(registry.has_tap("tap_perm"));
}

/// Test tap handler ordering by weight.
#[test]
fn tap_registry_handlers_in_weight_order() {
    let mut runtime =
        PluginRuntime::new(&PluginConfig::default()).expect("Failed to create runtime");

    let plugin_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
        .join("trovato_blog");

    runtime
        .load_plugin(&plugin_dir)
        .expect("Failed to load blog");

    let registry = TapRegistry::from_plugins(&runtime);

    let handlers = registry.get_handlers("tap_item_view");
    assert_eq!(handlers.len(), 1);
    assert_eq!(handlers[0].plugin.info.name, "trovato_blog");
    assert_eq!(handlers[0].weight, 0);
}

/// Test tap registry with no plugins.
#[test]
fn tap_registry_empty_when_no_plugins() {
    let runtime = PluginRuntime::new(&PluginConfig::default()).expect("Failed to create runtime");

    let registry = TapRegistry::from_plugins(&runtime);

    assert_eq!(registry.tap_count(), 0);
    assert!(!registry.has_tap("tap_item_view"));
    assert!(registry.get_handlers("tap_item_view").is_empty());
}

// =============================================================================
// RequestState Integration Tests
// =============================================================================

/// Test RequestState for anonymous user.
#[test]
fn request_state_anonymous_user() {
    let state = RequestState::default();

    assert_eq!(state.user.id, Uuid::nil());
    assert!(!state.user.authenticated);
    assert!(!state.has_services());
}

/// Test RequestState for authenticated user.
#[test]
fn request_state_authenticated_user() {
    let user_id = Uuid::new_v4();
    let perms = vec!["admin".to_string(), "edit_content".to_string()];
    let user = UserContext::authenticated(user_id, perms);
    let state = RequestState::without_services(user);

    assert_eq!(state.user.id, user_id);
    assert!(state.user.authenticated);
    assert!(state.user.has_permission("admin"));
    assert!(state.user.has_permission("edit_content"));
    assert!(!state.user.has_permission("delete_content"));
}

/// Test RequestState context key-value store.
#[test]
fn request_state_context_store() {
    let mut state = RequestState::default();

    // Initially empty
    assert!(state.get_context("request_id").is_none());

    // Set values
    state.set_context("request_id".to_string(), "abc123".to_string());
    state.set_context("locale".to_string(), "en-US".to_string());

    // Retrieve values
    assert_eq!(state.get_context("request_id"), Some("abc123"));
    assert_eq!(state.get_context("locale"), Some("en-US"));

    // Overwrite value
    state.set_context("request_id".to_string(), "xyz789".to_string());
    assert_eq!(state.get_context("request_id"), Some("xyz789"));
}

// =============================================================================
// Host Functions Integration Tests
// =============================================================================

/// Test that all host functions can be registered successfully.
#[test]
fn host_functions_register_all() {
    let config = wasmtime::Config::new();
    let engine = Engine::new(&config).unwrap();
    let mut linker: Linker<PluginState> = Linker::new(&engine);

    let result = host::register_all(&mut linker);
    assert!(
        result.is_ok(),
        "Failed to register host functions: {:?}",
        result.err()
    );
}

/// Round-trip test for the three host interfaces promoted into the published
/// contract (`crypto-api`, `http`, `queue`).
///
/// A small WASM module imports each interface under its registered module +
/// field name — exactly as the SDK bindings declare them — and calls each
/// through the real kernel linker. Successful instantiation proves the linker
/// grants precisely what the WIT/SDK contract declares; the assertions prove
/// the calls reach the live host implementations:
///
/// - `crypto-api.sha256` is pure, so we assert the actual SHA-256("abc") hash.
/// - `http.request` returns `ERR_NO_SERVICES` because the test request context
///   has no services attached (the call reaches the host and runs).
/// - `queue.push` returns `-4` (invalid payload JSON) — rejected before any DB
///   access, so no service/DB setup is required to prove reachability.
#[tokio::test]
async fn promoted_host_interfaces_roundtrip() {
    // wasmtime 43 always supports the async store/instantiate path
    // (`Config::async_support` is a deprecated no-op), matching how the
    // production dispatcher instantiates plugins via `instantiate_async`.
    let engine = Engine::new(&wasmtime::Config::new()).unwrap();

    let mut linker: Linker<PluginState> = Linker::new(&engine);
    host::register_all(&mut linker).expect("register host functions");

    // Imports use the exact (module, field) names the host registers and the
    // SDK links against: crypto fields are snake_case, http/queue are bare.
    let wat = r#"
    (module
      (import "trovato:kernel/crypto-api" "sha256"
        (func $sha256 (param i32 i32 i32 i32) (result i32)))
      (import "trovato:kernel/http" "request"
        (func $http (param i32 i32 i32 i32) (result i32)))
      (import "trovato:kernel/queue" "push"
        (func $queue_push (param i32 i32 i32 i32) (result i32)))
      (memory (export "memory") 1)
      (data (i32.const 0) "abc")
      (data (i32.const 16) "{}")
      (data (i32.const 32) "q")
      (data (i32.const 48) "nope")
      (func (export "run_sha256") (result i32)
        i32.const 0 i32.const 3 i32.const 1024 i32.const 64 call $sha256)
      (func (export "run_http") (result i32)
        i32.const 16 i32.const 2 i32.const 2048 i32.const 256 call $http)
      (func (export "run_queue") (result i32)
        i32.const 32 i32.const 1 i32.const 48 i32.const 4 call $queue_push))
    "#;

    let module = Module::new(&engine, wat).expect("compile WAT module");
    let state = PluginState::new(RequestState::default(), "roundtrip_test".to_string());
    let mut store = Store::new(&engine, state);

    let instance = linker
        .instantiate_async(&mut store, &module)
        .await
        .expect("module instantiates against the kernel linker");

    let memory = instance
        .get_memory(&mut store, "memory")
        .expect("module exports memory");

    // crypto-api: real SHA-256("abc").
    let run_sha256 = instance
        .get_typed_func::<(), i32>(&mut store, "run_sha256")
        .unwrap();
    let written = run_sha256.call_async(&mut store, ()).await.unwrap();
    assert_eq!(written, 64, "sha256 should write 64 hex chars");
    let mut hex = [0u8; 64];
    memory.read(&store, 1024, &mut hex).unwrap();
    assert_eq!(
        std::str::from_utf8(&hex).unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        "crypto-api sha256 round-trip produced the wrong hash"
    );

    // http: reachable through the linker; reports no services in this context.
    let run_http = instance
        .get_typed_func::<(), i32>(&mut store, "run_http")
        .unwrap();
    let http_code = run_http.call_async(&mut store, ()).await.unwrap();
    assert_eq!(
        http_code,
        trovato_sdk::host_errors::ERR_NO_SERVICES,
        "http.request should report no services without a request context"
    );

    // queue: reachable; invalid payload JSON is rejected (-4) before any DB access.
    let run_queue = instance
        .get_typed_func::<(), i32>(&mut store, "run_queue")
        .unwrap();
    let queue_code = run_queue.call_async(&mut store, ()).await.unwrap();
    assert_eq!(
        queue_code, -4,
        "queue.push should reject invalid payload JSON with -4"
    );
}

// =============================================================================
// D-20: WIT ⇄ host-const ⇄ linker consistency guard (PF-3 / D-1 / FR-2-F1)
//
// `crates/wit/kernel.wit` is a documentation contract — never fed to
// wit-bindgen, so nothing in the build keeps it truthful against the real ABI.
// These tests make it un-lie-able by tying all three planes together:
//   (a) the WIT `world plugin` import/export sets must equal the kernel's
//       `KNOWN_HOST_INTERFACES` / `KNOWN_TAPS` consts (under the kebab↔snake
//       mapping for taps); and
//   (b) every const-declared host interface (except a documented pending
//       allowlist) must actually be registered in the real `register_all`
//       linker — proven by instantiating a WAT that imports each one.
// Part (a) alone can't see (b): the const could name an interface the linker
// never wires. Together, WIT ⇄ const ⇄ linker cannot silently drift.
// =============================================================================

/// Path to the documentation-contract WIT, relative to this crate.
const KERNEL_WIT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../wit/kernel.wit");

/// Host interfaces declared in the WIT + `KNOWN_HOST_INTERFACES` whose host
/// implementation is not yet wired into `register_all`.
///
/// **Currently empty:** every known host interface is registered. `plugin-api`
/// (inter-plugin invoke, FR-4a / D-14) was registered by `register_plugin_api`
/// in Story 2.2, so it was dropped from this allowlist and the positive linker
/// tie below now proves its registration.
///
/// If a future interface lands in the WIT + const before its host impl, add it
/// here and restore a negative linker probe for it (see git history for the
/// `plugin-api` probe pattern).
const PENDING_HOST_INTERFACES: &[&str] = &[];

/// Parse the `import <iface>;` and `export tap-...:` lines from the WIT
/// `world plugin` block. Lightweight line scan rather than a real WIT parser:
/// the `.wit` is a documentation contract (not compiled), so pulling in
/// `wit-parser` solely for this guard would be overkill. Taps are returned in
/// the kernel's snake_case form so they compare directly against `KNOWN_TAPS`.
fn parse_wit_world(src: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut imports = BTreeSet::new();
    let mut taps = BTreeSet::new();
    for raw in src.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("import ") {
            // e.g. `import item-api;`
            let name = rest.trim_end_matches(';').trim();
            if !name.is_empty() {
                imports.insert(name.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("export tap-") {
            // e.g. `export tap-item-view: func(item-json: string) -> string;`
            if let Some((name, _)) = rest.split_once(':') {
                taps.insert(format!("tap_{}", name.trim().replace('-', "_")));
            }
        }
    }
    (imports, taps)
}

/// (a) The WIT `world plugin` surface must equal the kernel's const surface.
///
/// Fails loudly if a tap or host interface is added to one plane but not the
/// other — e.g. a new entry in `KNOWN_TAPS` without the matching WIT export, or
/// a stale WIT import that no longer has a const entry.
#[test]
fn wit_world_matches_known_host_interfaces_and_taps() {
    let src = std::fs::read_to_string(KERNEL_WIT_PATH).expect("read kernel.wit");
    let (wit_imports, wit_taps) = parse_wit_world(&src);

    let known_ifaces: BTreeSet<String> = KNOWN_HOST_INTERFACES
        .iter()
        .map(|s| s.to_string())
        .collect();
    let known_taps: BTreeSet<String> = KNOWN_TAPS.iter().map(|s| s.to_string()).collect();

    assert_eq!(
        wit_imports,
        known_ifaces,
        "WIT `world plugin` imports drifted from KNOWN_HOST_INTERFACES.\n  WIT-only:   {:?}\n  const-only: {:?}",
        wit_imports.difference(&known_ifaces).collect::<Vec<_>>(),
        known_ifaces.difference(&wit_imports).collect::<Vec<_>>(),
    );

    assert_eq!(
        wit_taps,
        known_taps,
        "WIT tap exports drifted from KNOWN_TAPS (kebab↔snake).\n  WIT-only:   {:?}\n  const-only: {:?}",
        wit_taps.difference(&known_taps).collect::<Vec<_>>(),
        known_taps.difference(&wit_taps).collect::<Vec<_>>(),
    );
}

/// (b) Every const-declared host interface except the pending allowlist must be
/// registered in the real `register_all` linker.
///
/// A WAT module imports one representative `(module, field, type)` per interface
/// — the exact names/arities the SDK links against — and instantiates against
/// the live kernel linker. Instantiation succeeds only if every import resolves,
/// proving each interface is actually wired (not merely named in the const/WIT).
#[tokio::test]
async fn every_known_host_interface_except_pending_is_registered() {
    let engine = Engine::new(&wasmtime::Config::new()).unwrap();
    let mut linker: Linker<PluginState> = Linker::new(&engine);
    host::register_all(&mut linker).expect("register host functions");

    // One representative function per registered interface. Field names and
    // arities mirror the host `func_wrap`/`func_wrap_async` registrations and
    // the SDK `#[link_name]` bindings exactly; crypto fields are kebab-case
    // post-D-21.
    let wat = r#"
    (module
      (import "trovato:kernel/item-api" "delete-item"
        (func (param i32 i32) (result i32)))
      (import "trovato:kernel/db" "select"
        (func (param i32 i32 i32 i32) (result i32)))
      (import "trovato:kernel/variables" "set"
        (func (param i32 i32 i32 i32) (result i32)))
      (import "trovato:kernel/request-context" "get"
        (func (param i32 i32 i32 i32) (result i32)))
      (import "trovato:kernel/user-api" "current-user-has-permission"
        (func (param i32 i32) (result i32)))
      (import "trovato:kernel/cache-api" "invalidate-tag"
        (func (param i32 i32)))
      (import "trovato:kernel/logging" "log"
        (func (param i32 i32 i32 i32 i32 i32)))
      (import "trovato:kernel/ai-api" "ai-request"
        (func (param i32 i32 i32 i32) (result i32)))
      (import "trovato:kernel/crypto-api" "sha256"
        (func (param i32 i32 i32 i32) (result i32)))
      (import "trovato:kernel/http" "request"
        (func (param i32 i32 i32 i32) (result i32)))
      (import "trovato:kernel/queue" "push"
        (func (param i32 i32 i32 i32) (result i32)))
      (import "trovato:kernel/plugin-api" "plugin-exists"
        (func (param i32 i32) (result i32)))
      (memory (export "memory") 1))
    "#;

    let module = Module::new(&engine, wat).expect("compile representative-imports WAT");
    let state = PluginState::new(RequestState::default(), "d20_linker_tie".to_string());
    let mut store = Store::new(&engine, state);
    linker
        .instantiate_async(&mut store, &module)
        .await
        .expect("every registered host interface resolves against the kernel linker");

    // Guard against silently skipping an interface: the representative set above
    // must cover exactly KNOWN_HOST_INTERFACES minus the pending allowlist.
    let covered: BTreeSet<&str> = [
        "item-api",
        "db",
        "variables",
        "request-context",
        "user-api",
        "cache-api",
        "logging",
        "ai-api",
        "crypto-api",
        "http",
        "queue",
        "plugin-api",
    ]
    .into_iter()
    .collect();
    let expected: BTreeSet<&str> = KNOWN_HOST_INTERFACES
        .iter()
        .copied()
        .filter(|i| !PENDING_HOST_INTERFACES.contains(i))
        .collect();
    assert_eq!(
        covered, expected,
        "representative-imports WAT must cover every non-pending host interface; \
         update the WAT when KNOWN_HOST_INTERFACES or the allowlist changes"
    );
}

/// (b, negative) The dispatch-pending allowlist must stay empty.
///
/// As of Story 2.2 every known host interface — including `plugin-api` — is
/// registered in `register_all`, so the positive tie above enforces all of them.
/// If a future interface is added to `PENDING_HOST_INTERFACES` (declared in the
/// WIT/const before its host impl lands), this tripwire fires: restore a real
/// negative linker probe for that interface (see git history for the `plugin-api`
/// probe) instead of leaving the gap unguarded.
#[test]
fn pending_allowlist_is_empty() {
    assert!(
        PENDING_HOST_INTERFACES.is_empty(),
        "a host interface is on the dispatch-pending allowlist ({PENDING_HOST_INTERFACES:?}); \
         add a negative linker probe asserting it is genuinely unregistered"
    );
}

// =============================================================================
// WASM-1: per-plugin linker (host-import gating, deny-unless-declared)
//
// Story 2.5 / D-18. These extend the D-20 guard for the move from one shared
// all-12 linker to a per-plugin linker built from each plugin's declared
// `host_interfaces`. They prove the gate actually gates — in both directions —
// and that the migration declarations satisfy the load-time import pre-check.
// =============================================================================

/// AC-6 (map completeness): the `HOST_INTERFACE_REGISTRARS` map keys must equal
/// `KNOWN_HOST_INTERFACES`.
///
/// The shared `register_all` carried "every known interface is registerable"
/// implicitly; under subset linking that invariant needs an explicit guard so
/// the map cannot silently drift from the const (e.g. a new interface added to
/// the const but never wired into the map, which would make it permanently
/// un-declarable).
#[test]
fn host_interface_registrar_map_covers_all_known() {
    let mapped: BTreeSet<&str> = host::HOST_INTERFACE_REGISTRARS
        .iter()
        .map(|(n, _)| *n)
        .collect();
    let known: BTreeSet<&str> = KNOWN_HOST_INTERFACES.iter().copied().collect();
    assert_eq!(
        mapped,
        known,
        "HOST_INTERFACE_REGISTRARS drifted from KNOWN_HOST_INTERFACES.\n  \
         map-only:   {:?}\n  const-only: {:?}",
        mapped.difference(&known).collect::<Vec<_>>(),
        known.difference(&mapped).collect::<Vec<_>>(),
    );
}

/// Build a linker exposing exactly `declared` (via `register_declared`, the
/// per-plugin path) and report whether `wat` instantiates against it — i.e.
/// whether every `trovato:kernel/*` import the module names resolves. WASI stubs
/// are intentionally NOT added: these probe modules import only host interfaces.
async fn declared_linker_instantiates(declared: &[&str], wat: &str) -> bool {
    let engine = Engine::new(&wasmtime::Config::new()).unwrap();
    let mut linker: Linker<PluginState> = Linker::new(&engine);
    let owned: Vec<String> = declared.iter().map(|s| (*s).to_string()).collect();
    host::register_declared(&mut linker, &owned).expect("register declared subset");
    let module = Module::new(&engine, wat).expect("compile probe WAT");
    let state = PluginState::new(RequestState::default(), "wasm1_subset_probe".to_string());
    let mut store = Store::new(&engine, state);
    linker.instantiate_async(&mut store, &module).await.is_ok()
}

/// AC-6 (positive + negative subset linking): `register_declared(["logging"])`
/// links `logging` and ONLY `logging`; `register_declared([])` links nothing —
/// replacing the coarse all-or-nothing of the shared linker.
#[tokio::test]
async fn register_declared_grants_only_named_subset() {
    let logging_wat = r#"
    (module
      (import "trovato:kernel/logging" "log"
        (func (param i32 i32 i32 i32 i32 i32)))
      (memory (export "memory") 1))
    "#;
    let db_wat = r#"
    (module
      (import "trovato:kernel/db" "select"
        (func (param i32 i32 i32 i32) (result i32)))
      (memory (export "memory") 1))
    "#;

    // Declared ["logging"] -> logging resolves, db does not.
    assert!(
        declared_linker_instantiates(&["logging"], logging_wat).await,
        "a plugin declaring [\"logging\"] must get the logging import linked"
    );
    assert!(
        !declared_linker_instantiates(&["logging"], db_wat).await,
        "a plugin declaring [\"logging\"] must NOT get the db import linked"
    );

    // Declared [] (== capabilities: None) -> nothing linked (deny-all).
    assert!(
        !declared_linker_instantiates(&[], logging_wat).await,
        "a plugin declaring [] (or None) must get no host interface linked"
    );
}

/// AC-7 (caller gate CLOSED): a plugin resolves `plugin-api` (`invoke` /
/// `plugin-exists`) ONLY if it declares `host_interfaces = ["plugin-api"]`. This
/// is the FR-4a caller-gate edge Story 2.2 left open — the shared linker granted
/// `plugin-api` to every plugin; the per-plugin linker no longer does.
#[tokio::test]
async fn caller_gate_plugin_api_requires_declaration() {
    let invoke_wat = r#"
    (module
      (import "trovato:kernel/plugin-api" "invoke"
        (func (param i32 i32 i32 i32 i32 i32 i32 i32) (result i64)))
      (memory (export "memory") 1))
    "#;
    let exists_wat = r#"
    (module
      (import "trovato:kernel/plugin-api" "plugin-exists"
        (func (param i32 i32) (result i32)))
      (memory (export "memory") 1))
    "#;

    // Undeclared, and a non-plugin-api declaration -> plugin-api unresolved.
    assert!(
        !declared_linker_instantiates(&[], invoke_wat).await,
        "a plugin without [\"plugin-api\"] must NOT resolve the invoke import"
    );
    assert!(
        !declared_linker_instantiates(&["logging"], invoke_wat).await,
        "declaring a different interface must NOT grant plugin-api"
    );
    assert!(
        !declared_linker_instantiates(&[], exists_wat).await,
        "a plugin without [\"plugin-api\"] must NOT resolve the plugin-exists import"
    );

    // Declared -> both plugin-api imports resolve.
    assert!(
        declared_linker_instantiates(&["plugin-api"], invoke_wat).await,
        "a plugin declaring [\"plugin-api\"] must resolve the invoke import"
    );
    assert!(
        declared_linker_instantiates(&["plugin-api"], exists_wat).await,
        "a plugin declaring [\"plugin-api\"] must resolve the plugin-exists import"
    );
}

/// AC-4 + AC-3 (load-time import pre-check, deny-all): a plugin whose compiled
/// module imports a host interface it does not declare fails to LOAD — not
/// mid-request — with a declarative message naming the plugin, the interface,
/// and the fix. Covers both `capabilities: None` and an explicit empty list
/// (both ⇒ deny-all).
#[test]
fn load_rejects_undeclared_host_import_with_declarative_error() {
    let db_importer_wat = r#"
    (module
      (import "trovato:kernel/db" "select"
        (func (param i32 i32 i32 i32) (result i32)))
      (memory (export "memory") 1))
    "#;

    for (label, caps) in [
        ("none", ""),
        ("empty", "\n[capabilities]\nhost_interfaces = []\n"),
    ] {
        let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("runtime");
        let dir = std::env::temp_dir().join(format!("trovato_wasm1_undeclared_{label}"));
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(
            dir.join("dbplug.info.toml"),
            format!("name = \"dbplug\"\ndescription = \"x\"\nversion = \"1.0.0\"\n{caps}"),
        )
        .unwrap();
        std::fs::write(dir.join("dbplug.wasm"), db_importer_wat).unwrap();

        let err = runtime
            .load_plugin(&dir)
            .expect_err("undeclared db import must fail the load")
            .to_string();
        assert!(
            err.contains("imports host interface 'db'")
                && err.contains("does not declare")
                && err.contains("dbplug"),
            "[{label}] expected declarative undeclared-import error, got: {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// AC-4 (positive): a plugin that DOES declare the interface it imports loads
/// cleanly through the pre-check (the gate admits the declared path).
#[test]
fn load_admits_declared_host_import() {
    let db_importer_wat = r#"
    (module
      (import "trovato:kernel/db" "select"
        (func (param i32 i32 i32 i32) (result i32)))
      (memory (export "memory") 1))
    "#;
    let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("runtime");
    let dir = std::env::temp_dir().join("trovato_wasm1_declared_ok");
    std::fs::create_dir_all(&dir).ok();
    std::fs::write(
        dir.join("dbplug.info.toml"),
        "name = \"dbplug\"\ndescription = \"x\"\nversion = \"1.0.0\"\n\
         \n[capabilities]\nhost_interfaces = [\"db\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("dbplug.wasm"), db_importer_wat).unwrap();

    runtime
        .load_plugin(&dir)
        .expect("a plugin declaring the db it imports must load");
    assert!(runtime.get_plugin("dbplug").is_some());
    std::fs::remove_dir_all(&dir).ok();
}

/// AC-5 reference-app load smoke: every in-tree plugin that ships a compiled
/// `.wasm` loads via its new declared `host_interfaces` subset. The migration
/// declarations were derived from each plugin's real `.wasm` imports, so the
/// load-time pre-check admits them; a missing/incorrect declaration would reject
/// the plugin and it would be absent here.
///
/// The set of built `.wasm` is environment-dependent — CI builds only
/// `trovato_blog` + `trovato_search` (the `.wasm` are gitignored), while a local
/// tree may have all of them — so the expectation is derived from what is
/// actually present rather than hardcoded. The two CI-built plugins still span
/// the meaningful profiles: `trovato_blog` (`["logging"]`) and `trovato_search`
/// (`["db", "logging"]` + `raw_sql`).
#[tokio::test]
async fn in_tree_plugins_load_with_declared_subsets() {
    let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("runtime");
    let plugins_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins");
    runtime.load_all(&plugins_dir).await.expect("load_all");

    // Every in-tree plugin dir that ships BOTH a manifest and a matching compiled
    // `<name>.wasm` must have loaded under its declared subset. (`ritrovo_importer`
    // is `.wasm`-only with no in-tree manifest, so it is skipped here — its
    // declaration lives in the ritrovo repo, which owns and builds it. It is the
    // last in-tree ritrovo artifact; FR-25 removes it.)
    let mut checked = 0;
    for entry in std::fs::read_dir(&plugins_dir).expect("read plugins dir") {
        let dir = entry.expect("dir entry").path();
        if !dir.is_dir() {
            continue;
        }
        let Some(name) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        let has_manifest = dir.join(format!("{name}.info.toml")).exists();
        let has_wasm = dir.join(format!("{name}.wasm")).exists();
        if has_manifest && has_wasm {
            assert!(
                runtime.get_plugin(&name).is_some(),
                "{name} ships a manifest + compiled .wasm but failed to load under its \
                 declared host_interfaces subset; loaded: {:?}",
                runtime.plugins().keys().collect::<Vec<_>>()
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "expected at least one in-tree plugin with a built .wasm to load"
    );
}

/// Test that plugin runtime includes all host functions.
#[test]
fn plugin_runtime_has_host_functions() {
    let runtime = PluginRuntime::new(&PluginConfig::default()).expect("Failed to create runtime");

    // The linker should have host functions registered
    // We can't easily inspect the linker contents, but we can verify
    // the runtime was created successfully with the host module integrated
    assert!(runtime.plugin_count() == 0); // No plugins loaded yet
}

// =============================================================================
// Tap Dispatcher Integration Tests
// =============================================================================

/// Test creating a tap dispatcher.
#[test]
fn tap_dispatcher_creation() {
    let runtime =
        Arc::new(PluginRuntime::new(&PluginConfig::default()).expect("Failed to create runtime"));
    let registry = Arc::new(TapRegistry::from_plugins(&runtime));
    let _dispatcher = TapDispatcher::new(runtime, registry);
}

/// Test dispatching to non-existent tap returns empty.
#[tokio::test]
async fn tap_dispatcher_empty_result() {
    let runtime =
        Arc::new(PluginRuntime::new(&PluginConfig::default()).expect("Failed to create runtime"));
    let registry = Arc::new(TapRegistry::from_plugins(&runtime));
    let dispatcher = TapDispatcher::new(runtime, registry);

    let results = dispatcher
        .dispatch("tap_nonexistent", "{}", RequestState::default())
        .await;

    assert!(results.is_empty());
}

// =============================================================================
// Menu Registry Integration Tests
// =============================================================================

/// Test creating menu registry from JSON.
#[test]
fn menu_registry_from_json() {
    let json = r#"[
        {"path": "/admin", "title": "Admin"},
        {"path": "/admin/content", "title": "Content", "parent": "/admin"}
    ]"#;

    let registry = MenuRegistry::from_tap_results(vec![("admin".to_string(), json.to_string())]);

    assert_eq!(registry.len(), 2);
    assert!(registry.get("/admin").is_some());
    assert!(registry.get("/admin/content").is_some());
}

/// Test menu path matching with parameters.
#[test]
fn menu_registry_path_matching() {
    let json = r#"[
        {"path": "/blog", "title": "Blog"},
        {"path": "/blog/:slug", "title": "Post"}
    ]"#;

    let registry =
        MenuRegistry::from_tap_results(vec![("trovato_blog".to_string(), json.to_string())]);

    // Exact match
    let result = registry.match_path("/blog");
    assert!(result.is_some());
    assert_eq!(result.unwrap().menu.path, "/blog");

    // Parameter match
    let result = registry.match_path("/blog/hello-world");
    assert!(result.is_some());
    let result = result.unwrap();
    assert_eq!(result.menu.path, "/blog/:slug");
    assert_eq!(result.params.get("slug"), Some(&"hello-world".to_string()));
}

// =============================================================================
// Dependency Resolution Integration Tests
// =============================================================================

/// Test resolving plugin load order with no dependencies.
#[test]
fn dependency_resolution_no_deps() {
    use trovato_kernel::plugin::TapConfig;

    let mut plugins = HashMap::new();
    plugins.insert(
        "a".to_string(),
        PluginInfo {
            name: "a".to_string(),
            description: "Plugin A".to_string(),
            version: "1.0.0".to_string(),
            api_version: "0.2".to_string(),
            default_enabled: true,
            dependencies: vec![],
            taps: TapConfig::default(),
            migrations: trovato_kernel::plugin::MigrationConfig::default(),
            capabilities: None,
            record_types: vec![],
        },
    );
    plugins.insert(
        "b".to_string(),
        PluginInfo {
            name: "b".to_string(),
            description: "Plugin B".to_string(),
            version: "1.0.0".to_string(),
            api_version: "0.2".to_string(),
            default_enabled: true,
            dependencies: vec![],
            taps: TapConfig::default(),
            migrations: trovato_kernel::plugin::MigrationConfig::default(),
            capabilities: None,
            record_types: vec![],
        },
    );

    let order = resolve_load_order(&plugins).expect("Failed to resolve");
    assert_eq!(order.len(), 2);
}

/// Test resolving plugin load order respects dependencies.
#[test]
fn dependency_resolution_with_deps() {
    use trovato_kernel::plugin::TapConfig;

    let mut plugins = HashMap::new();
    plugins.insert(
        "base".to_string(),
        PluginInfo {
            name: "base".to_string(),
            description: "Base Plugin".to_string(),
            version: "1.0.0".to_string(),
            api_version: "0.2".to_string(),
            default_enabled: true,
            dependencies: vec![],
            taps: TapConfig::default(),
            migrations: trovato_kernel::plugin::MigrationConfig::default(),
            capabilities: None,
            record_types: vec![],
        },
    );
    plugins.insert(
        "child".to_string(),
        PluginInfo {
            name: "child".to_string(),
            description: "Child Plugin".to_string(),
            version: "1.0.0".to_string(),
            api_version: "0.2".to_string(),
            default_enabled: true,
            dependencies: vec!["base".to_string()],
            taps: TapConfig::default(),
            migrations: trovato_kernel::plugin::MigrationConfig::default(),
            capabilities: None,
            record_types: vec![],
        },
    );

    let order = resolve_load_order(&plugins).expect("Failed to resolve");

    let base_pos = order.iter().position(|x| x == "base").unwrap();
    let child_pos = order.iter().position(|x| x == "child").unwrap();

    assert!(base_pos < child_pos, "base must load before child");
}

/// Test circular dependency detection.
#[test]
fn dependency_resolution_circular() {
    use trovato_kernel::plugin::TapConfig;

    let mut plugins = HashMap::new();
    plugins.insert(
        "a".to_string(),
        PluginInfo {
            name: "a".to_string(),
            description: "Plugin A".to_string(),
            version: "1.0.0".to_string(),
            api_version: "0.2".to_string(),
            default_enabled: true,
            dependencies: vec!["b".to_string()],
            taps: TapConfig::default(),
            migrations: trovato_kernel::plugin::MigrationConfig::default(),
            capabilities: None,
            record_types: vec![],
        },
    );
    plugins.insert(
        "b".to_string(),
        PluginInfo {
            name: "b".to_string(),
            description: "Plugin B".to_string(),
            version: "1.0.0".to_string(),
            api_version: "0.2".to_string(),
            default_enabled: true,
            dependencies: vec!["a".to_string()],
            taps: TapConfig::default(),
            migrations: trovato_kernel::plugin::MigrationConfig::default(),
            capabilities: None,
            record_types: vec![],
        },
    );

    let result = resolve_load_order(&plugins);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("circular"));
}

// =============================================================================
// WASM-2: db-table allowlist enforcement (D-19 Option A)
//
// End-to-end through the real host `select` implementation and the real
// per-plugin linker: a WAT plugin that imports `trovato:kernel/db` `select` and
// targets a table outside its effective allowlist is rejected with
// `ERR_TABLE_NOT_DECLARED` (-16) *before* any pool access — so this probe needs
// no live Postgres. A select against a migration-owned table passes the gate
// (and then fails on the unconnectable lazy pool, proving the gate let it
// through rather than blocking it).
// =============================================================================

/// A module that selects from `forbidden_t` (not in the allowlist) via the real
/// `db.select` host function and returns the host's i32 result code.
const DB_SELECT_FORBIDDEN_WAT: &str = r#"
(module
  (import "trovato:kernel/db" "select"
    (func $select (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 2)
  (data (i32.const 0) "{\"table\":\"forbidden_t\"}")
  (func (export "run") (result i32)
    (call $select (i32.const 0) (i32.const 23) (i32.const 1024) (i32.const 512))))
"#;

/// Same, but targets `allowed_t` — a migration-owned table in the derived policy.
const DB_SELECT_ALLOWED_WAT: &str = r#"
(module
  (import "trovato:kernel/db" "select"
    (func $select (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 2)
  (data (i32.const 0) "{\"table\":\"allowed_t\"}")
  (func (export "run") (result i32)
    (call $select (i32.const 0) (i32.const 21) (i32.const 1024) (i32.const 512))))
"#;

/// Build a `PluginState` whose DB policy is *derived* (the real load path) from a
/// temp fixture whose only migration is `CREATE TABLE allowed_t`, carrying
/// services with a lazy (never-connected) pool so the allowlist gate — not a DB
/// error — is what we observe.
fn db_probe_state(plugin: &str) -> PluginState {
    use trovato_kernel::plugin::{MigrationConfig, PluginCapabilities, TapConfig};

    // A directory of this call's own. The path used to be derived from the plugin name
    // alone, and both callers pass the same name, so the two probes shared one
    // directory — and each one *deletes* it after `DbPolicy::derive` has read it. The
    // losing interleaving is:
    //
    //   1. probe A writes the migration
    //   2. probe B writes the migration
    //   3. probe A derives its policy (sees `allowed_t`)
    //   4. probe A removes the directory
    //   5. probe B derives its policy — the file is gone, so the allowlist is empty
    //      and its in-allowlist select is rejected with ERR_TABLE_NOT_DECLARED
    //
    // which fails `db_select_inside_allowlist_passes_gate` intermittently, and always
    // for a reason that has nothing to do with the gate it is testing.
    static PROBE_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = PROBE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "trovato_wasm2_probe_{plugin}_{}_{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("migrations")).expect("create fixture migrations dir");
    std::fs::write(
        dir.join("migrations/001_init.sql"),
        "CREATE TABLE allowed_t (id bigint primary key);",
    )
    .expect("write migration");

    let info = PluginInfo {
        name: plugin.to_string(),
        description: "wasm2 probe".to_string(),
        version: "1.0.0".to_string(),
        api_version: "0.2".to_string(),
        default_enabled: true,
        dependencies: vec![],
        taps: TapConfig::default(),
        migrations: MigrationConfig {
            files: vec!["migrations/001_init.sql".to_string()],
            depends_on: vec![],
        },
        capabilities: Some(PluginCapabilities {
            host_interfaces: vec!["db".to_string()],
            db_tables: vec![],
            raw_sql: false,
            ai_background: false,
            http_max_transfer: None,
            public_functions: vec![],
        }),
        record_types: vec![],
    };
    let policy = DbPolicy::derive(&info, &dir);
    std::fs::remove_dir_all(&dir).ok();

    let db =
        sqlx::postgres::PgPool::connect_lazy("postgres://localhost/trovato").expect("lazy pool");
    let services = RequestServices::for_background(db, None, None, reqwest::Client::new());
    let request = RequestState::new(UserContext::anonymous(), services);
    PluginState::with_db_policy(
        request,
        plugin.to_string(),
        Arc::new(policy),
        ResourceLimits::default(),
    )
}

async fn run_db_select_probe(wat: &str, plugin: &str) -> i32 {
    let engine = Engine::new(&wasmtime::Config::new()).unwrap();
    let mut linker: Linker<PluginState> = Linker::new(&engine);
    host::register_all(&mut linker).expect("register host functions");

    let module = Module::new(&engine, wat).expect("compile db-select probe WAT");
    let mut store = Store::new(&engine, db_probe_state(plugin));
    let instance = linker
        .instantiate_async(&mut store, &module)
        .await
        .expect("module instantiates against the kernel linker");
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .expect("module exports run");
    run.call_async(&mut store, ()).await.expect("run completes")
}

/// A structured select against a table outside the effective allowlist is
/// rejected end-to-end with the exact WASM-2 ABI code, without touching the DB.
#[tokio::test]
async fn db_select_outside_allowlist_is_rejected() {
    let code = run_db_select_probe(DB_SELECT_FORBIDDEN_WAT, "wasm2_probe").await;
    assert_eq!(
        code,
        trovato_sdk::host_errors::ERR_TABLE_NOT_DECLARED,
        "out-of-allowlist select must be rejected with ERR_TABLE_NOT_DECLARED (-16)"
    );
}

/// A structured select against a migration-owned table passes the allowlist gate
/// (it then fails on the unconnectable lazy pool — proving the gate allowed it
/// rather than blocking it: the code is NOT the table-not-declared rejection).
#[tokio::test]
async fn db_select_inside_allowlist_passes_gate() {
    let code = run_db_select_probe(DB_SELECT_ALLOWED_WAT, "wasm2_probe").await;
    assert_ne!(
        code,
        trovato_sdk::host_errors::ERR_TABLE_NOT_DECLARED,
        "a migration-owned table must pass the allowlist gate"
    );
}
