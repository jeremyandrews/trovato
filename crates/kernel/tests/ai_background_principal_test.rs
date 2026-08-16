#![allow(clippy::unwrap_used, clippy::expect_used)]
//! P11c (D-40 / D-41) — background-AI principal validation through **real**
//! dispatch.
//!
//! Drives `plugins/test_ai_background` (built from Rust with the real
//! `trovato-plugin-sdk` and the real `wasm32-wasip1` toolchain) through the real
//! kernel path: `TapDispatcher::dispatch_to_plugin("tap_cron", …)` → the plugin
//! WASM → the `ai-request` host function → back. The fixture calls `ai_request`
//! from `tap_cron` and returns the host result code, so these tests observe the
//! authorization decision end-to-end rather than through a stub.
//!
//! Two `RequestState` shapes exercise the two authorization planes against the
//! **same** capability-holding plugin:
//!
//! - **Background principal** (`UserContext::background()`): the `ai_background`
//!   capability clears authorization, so the call proceeds to provider
//!   resolution. No provider is configured against the (never-connected) test
//!   pool, so the host returns `ERR_AI_NO_PROVIDER` — proof that authorization
//!   passed rather than being denied.
//! - **Anonymous web** (`UserContext::anonymous()`): the permission gate denies
//!   the call with `ERR_AI_PERMISSION_DENIED` **before** provider resolution,
//!   proving the web denial is byte-for-byte intact even for a plugin that holds
//!   the background capability.
//!
//! The two codes are distinct (`-20` vs `-27`), so a background principal and a
//! web caller are unambiguously separable through the real host path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::services::ai_provider::AiProviderService;
use trovato_kernel::tap::{RequestServices, RequestState, TapDispatcher, TapRegistry, UserContext};
use trovato_sdk::host_errors;

/// Repo `plugins/` directory (two levels up from this crate).
fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

/// Build a dispatcher with only the background-AI fixture loaded.
fn dispatcher_with_fixture() -> Arc<TapDispatcher> {
    let name = "test_ai_background";
    let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
    runtime
        .load_plugin(&plugins_dir().join(name))
        .unwrap_or_else(|e| {
            panic!(
                "failed to load fixture '{name}': {e:#}\n\
                 build it first: cargo build -p {name} --target wasm32-wasip1 --release \
                 && cp target/wasm32-wasip1/release/{name}.wasm plugins/{name}/"
            )
        });
    let runtime = Arc::new(runtime);
    let registry = Arc::new(TapRegistry::from_plugins(&runtime));
    Arc::new(TapDispatcher::new(Arc::clone(&runtime), registry))
}

/// A never-connected lazy pool: the AI provider service is present (so the host
/// clears the "no AI service" check), but has no configured provider, so provider
/// resolution fails with `ERR_AI_NO_PROVIDER` for any call that clears authz.
fn lazy_pool() -> sqlx::PgPool {
    sqlx::postgres::PgPool::connect_lazy("postgres://localhost/trovato").expect("lazy pool")
}

/// Build a background `RequestState` whose services carry a present-but-empty AI
/// provider service and the fixture's runtime (for tap dispatch).
fn state_with(user: UserContext, runtime: Arc<PluginRuntime>) -> RequestState {
    let db = lazy_pool();
    let ai_providers = Some(Arc::new(AiProviderService::new(db.clone())));
    let services = RequestServices::for_background(db, ai_providers, None, reqwest::Client::new())
        .with_plugin_runtime(runtime);
    RequestState::new(user, services)
}

/// Extract the `ai_code` the fixture reported, or fail loudly.
fn ai_code(output: &str) -> i64 {
    let v: serde_json::Value = serde_json::from_str(output).expect("fixture output is JSON");
    v.get("ai_code")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_else(|| panic!("expected an ai_code in fixture output, got: {output}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn background_principal_with_capability_passes_authorization() {
    let dispatcher = dispatcher_with_fixture();
    let runtime = dispatcher.runtime().clone();
    let state = state_with(UserContext::background(), runtime);

    let result = dispatcher
        .dispatch_to_plugin("tap_cron", "{}", "test_ai_background", state)
        .await
        .expect("fixture implements tap_cron");

    // Capability holds ⇒ authorization clears; with no provider configured the
    // host reaches resolution and returns ERR_AI_NO_PROVIDER — NOT a denial.
    assert_eq!(
        ai_code(&result.output),
        host_errors::ERR_AI_NO_PROVIDER as i64,
        "background principal with ai_background must clear authz (reach provider resolution), \
         not be denied"
    );
    assert_ne!(
        ai_code(&result.output),
        host_errors::ERR_AI_BACKGROUND_DENIED as i64
    );
    assert_ne!(
        ai_code(&result.output),
        host_errors::ERR_AI_PERMISSION_DENIED as i64
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn anonymous_web_request_still_denied_even_for_capable_plugin() {
    let dispatcher = dispatcher_with_fixture();
    let runtime = dispatcher.runtime().clone();
    // Anonymous web caller (NOT a background principal) — the exact pre-P11c
    // denial must still fire, ahead of provider resolution.
    let state = state_with(UserContext::anonymous(), runtime);

    let result = dispatcher
        .dispatch_to_plugin("tap_cron", "{}", "test_ai_background", state)
        .await
        .expect("fixture implements tap_cron");

    assert_eq!(
        ai_code(&result.output),
        host_errors::ERR_AI_PERMISSION_DENIED as i64,
        "an anonymous web caller must be denied at the permission gate even when the plugin \
         holds the ai_background capability"
    );
}
