#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The AI Assistant, driven through the real router (0.102).
//!
//! Everything here runs against the **real** `plugins/test_assistant_scope`
//! wasm, the real tap dispatcher and a real Postgres. The model is the one thing
//! that is not real: a scripted provider
//! ([`trovato_kernel::services::ai_tools::scripted_provider`]) stands in for it,
//! written straight into `site_config.ai_providers`, so a test can say "now the
//! model calls `read_widget`, then answers with text" and watch what the kernel
//! does about it.
//!
//! That the provider's `base_url` is a loopback address is deliberate and is the
//! seam these tests use: the admin form refuses a loopback URL as SSRF
//! prevention, and the chat paths do not re-validate at call time, so writing
//! the config row directly reaches the same code a real provider would.
//!
//! `test_assistant_scope` is `default_enabled = false`, so this file installs it
//! and builds its **own** `TestApp`: `AppState` resolves the enabled plugin set,
//! and therefore the assistant scope registry, at construction.
//!
//! Requires Postgres + Redis and the fixture `.wasm` built into
//! `plugins/test_assistant_scope/`.

mod common;

use common::TestApp;

const PLUGIN: &str = "test_assistant_scope";
const SCOPE: &str = "test_widget";

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("assistant fixture app init thread panicked")
    })
}

async fn build_app() -> TestApp {
    trovato_test_utils::env::load_dotenv();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("failed to connect for fixture setup");
    trovato_kernel::plugin::status::install_plugin(&pool, PLUGIN, "1.0.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{PLUGIN}': {e:#}"));
    pool.close().await;

    // Integration tests run with the crate directory as CWD, so `Config`'s
    // default `./plugins` resolves to nothing.
    TestApp::with_config(|config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
    })
    .await
}

// =============================================================================
// The registry, built at boot from the real plugin
// =============================================================================

#[test]
fn the_valid_scope_is_registered_and_carries_its_tools() {
    common::run_test(async {
        let app = app();
        let registry = app.state.assistant_scopes();

        let registered = registry
            .get(SCOPE)
            .expect("the fixture's valid scope must be registered");
        assert_eq!(registered.plugin, PLUGIN);
        assert_eq!(registered.scope.label, "Test widget");
        assert_eq!(registered.scope.permission, "configure test widget");
        assert_eq!(registered.scope.suggestions.len(), 2);
        assert!(registered.tool("read_widget").is_some());
        assert!(registered.tool("set_widget_color").is_some());
        assert!(registered.tool("fail_loudly").is_some());
        assert_eq!(registered.write_tool_count(), 1);
    });
}

#[test]
fn the_invalid_scope_is_dropped_and_the_rejection_names_the_reason() {
    common::run_test(async {
        let app = app();
        let registry = app.state.assistant_scopes();

        // One bad declaration must not take the good one with it, and must not
        // stop the site booting — which it demonstrably did not, since this app
        // exists.
        assert!(
            registry.get("broken_widget").is_none(),
            "a scope with a tool name containing a space must be dropped"
        );

        let rejection = registry
            .rejections()
            .iter()
            .find(|r| r.scope == "broken_widget")
            .expect("the drop must be recorded, not silent");
        assert_eq!(rejection.plugin, PLUGIN);
        assert!(
            rejection.reason.contains("read widget"),
            "the reason must name the offending tool: {}",
            rejection.reason
        );
    });
}
