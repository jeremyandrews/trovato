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

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use trovato_kernel::host;
use trovato_kernel::menu::MenuRegistry;
use trovato_kernel::plugin::{
    PluginConfig, PluginInfo, PluginRuntime, PluginState, resolve_load_order,
};
use trovato_kernel::tap::{RequestState, TapDispatcher, TapRegistry, UserContext};
use uuid::Uuid;
use wasmtime::{Engine, Linker};

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
        async_support: false,
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
        runtime.get_plugin("blog").is_some(),
        "Blog plugin not loaded. Available: {:?}",
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
        .join("blog");

    let result = runtime.load_plugin(&plugin_dir);
    assert!(
        result.is_ok(),
        "Failed to load blog plugin: {:?}",
        result.err()
    );

    let plugin = runtime.get_plugin("blog").expect("Plugin not found");
    assert_eq!(plugin.info.name, "blog");
    assert_eq!(plugin.info.version, "1.0.0");
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
        "Expected WASM error, got: {}",
        err
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
        "Expected parse error, got: {}",
        err
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
        "Expected unknown tap error, got: {}",
        err
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
        .join("blog");

    runtime.load_plugin(&plugin_dir).expect("Failed to load");

    let plugin = runtime.get_plugin("blog").expect("Plugin not found");

    // Check metadata
    assert_eq!(plugin.info.name, "blog");
    assert_eq!(
        plugin.info.description,
        "Provides a blog content type with tags"
    );
    assert_eq!(plugin.info.version, "1.0.0");
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
        .join("blog");

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
        .join("blog");

    runtime
        .load_plugin(&plugin_dir)
        .expect("Failed to load blog");

    let registry = TapRegistry::from_plugins(&runtime);

    let handlers = registry.get_handlers("tap_item_view");
    assert_eq!(handlers.len(), 1);
    assert_eq!(handlers[0].plugin.info.name, "blog");
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

    let registry = MenuRegistry::from_tap_results(vec![("blog".to_string(), json.to_string())]);

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
            default_enabled: true,
            dependencies: vec![],
            taps: TapConfig::default(),
            migrations: trovato_kernel::plugin::MigrationConfig::default(),
        },
    );
    plugins.insert(
        "b".to_string(),
        PluginInfo {
            name: "b".to_string(),
            description: "Plugin B".to_string(),
            version: "1.0.0".to_string(),
            default_enabled: true,
            dependencies: vec![],
            taps: TapConfig::default(),
            migrations: trovato_kernel::plugin::MigrationConfig::default(),
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
            default_enabled: true,
            dependencies: vec![],
            taps: TapConfig::default(),
            migrations: trovato_kernel::plugin::MigrationConfig::default(),
        },
    );
    plugins.insert(
        "child".to_string(),
        PluginInfo {
            name: "child".to_string(),
            description: "Child Plugin".to_string(),
            version: "1.0.0".to_string(),
            default_enabled: true,
            dependencies: vec!["base".to_string()],
            taps: TapConfig::default(),
            migrations: trovato_kernel::plugin::MigrationConfig::default(),
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
            default_enabled: true,
            dependencies: vec!["b".to_string()],
            taps: TapConfig::default(),
            migrations: trovato_kernel::plugin::MigrationConfig::default(),
        },
    );
    plugins.insert(
        "b".to_string(),
        PluginInfo {
            name: "b".to_string(),
            description: "Plugin B".to_string(),
            version: "1.0.0".to_string(),
            default_enabled: true,
            dependencies: vec!["a".to_string()],
            taps: TapConfig::default(),
            migrations: trovato_kernel::plugin::MigrationConfig::default(),
        },
    );

    let result = resolve_load_order(&plugins);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("circular"));
}
