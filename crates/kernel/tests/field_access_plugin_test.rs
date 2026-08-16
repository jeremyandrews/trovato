#![allow(clippy::unwrap_used, clippy::expect_used)]
//! FR-8 Story 3.8 — freeze-supporting validation of the frozen `tap_field_access`
//! batch schema through the **real** reference plugin.
//!
//! This drives `plugins/trovato_field_access_ref` (built from Rust with the real
//! `trovato-plugin-sdk` and the real `wasm32-wasip1` toolchain) through the real
//! kernel path: `ItemService::field_access_decisions` → `TapDispatcher::dispatch`
//! → the plugin WASM → back through the deny-wins + fail-open aggregation
//! (design §2.3). It proves the frozen `FieldAccessBatchInput` /
//! `FieldAccessBatchResult` shape round-trips through a genuine SDK plugin —
//! the tap-csp-alter / Story-2.4 discipline: never freeze an unexercised payload.
//!
//! # No infrastructure required
//!
//! The plugin reads its `field_rules` from the `variables` host function; with a
//! lazy, never-connected pool the read falls back to the plugin's baked-in
//! [`DEFAULT_RULES`], so these assertions exercise the default rule set without
//! Postgres/Redis. The tests therefore always run; CI builds the fixture.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use std::time::Duration;
use trovato_kernel::content::ItemService;
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::tap::{RequestServices, TapDispatcher, TapRegistry, UserContext};

/// Repo `plugins/` directory (two levels up from this crate).
fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

/// Build an `ItemService` whose dispatcher has the reference plugin loaded.
///
/// Panics with a build hint if the fixture `.wasm` is missing — CI builds it
/// before the test job; locally `cargo build -p trovato_field_access_ref
/// --target wasm32-wasip1 --release && cp …` is the same step.
fn item_service_with_ref_plugin() -> ItemService {
    let name = "trovato_field_access_ref";
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
    let dispatcher = Arc::new(TapDispatcher::new(Arc::clone(&runtime), registry));

    // Lazy pool: the plugin's variables_get falls back to DEFAULT_RULES when the
    // read errors, so no live Postgres is needed.
    let db =
        sqlx::postgres::PgPool::connect_lazy("postgres://localhost/trovato").expect("lazy pool");
    let services = RequestServices::for_background(db.clone(), None, None, reqwest::Client::new())
        .with_plugin_runtime(Arc::clone(&runtime));

    ItemService::new(
        db,
        dispatcher,
        services,
        Duration::from_secs(60),
        None,
        None,
    )
}

fn user(perms: &[&str]) -> UserContext {
    UserContext::authenticated(
        uuid::Uuid::now_v7(),
        perms.iter().map(|s| s.to_string()).collect(),
    )
}

fn fields(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn ritrovo_role_pattern_denies_field_without_permission() {
    let items = item_service_with_ref_plugin();
    // Viewer holds "view salary" but not "view pii".
    let d = items
        .field_access_decisions(
            &user(&["view salary"]),
            "person",
            &fields(&["ssn", "salary", "bio"]),
            "view",
        )
        .await;
    assert_eq!(d.get("ssn"), Some(&false), "no 'view pii' ⇒ ssn denied");
    assert_eq!(
        d.get("salary"),
        Some(&true),
        "'view salary' ⇒ salary visible"
    );
    assert_eq!(d.get("bio"), Some(&true), "ungoverned ⇒ fail-open visible");
}

#[tokio::test(flavor = "multi_thread")]
async fn cairn_tier_pattern_denies_field_above_clearance() {
    let items = item_service_with_ref_plugin();
    // Clearance 3: sees tier-3 field, not tier-5.
    let d = items
        .field_access_decisions(
            &user(&["clearance 3"]),
            "record",
            &fields(&["secret_notes", "top_secret", "summary"]),
            "view",
        )
        .await;
    assert_eq!(d.get("secret_notes"), Some(&true), "tier 3 ≤ clearance 3");
    assert_eq!(d.get("top_secret"), Some(&false), "tier 5 > clearance 3");
    assert_eq!(d.get("summary"), Some(&true), "ungoverned ⇒ fail-open");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_clearance_denies_all_tiered_fields() {
    let items = item_service_with_ref_plugin();
    let d = items
        .field_access_decisions(
            &user(&["access content"]),
            "record",
            &fields(&["secret_notes", "top_secret"]),
            "view",
        )
        .await;
    assert_eq!(d.get("secret_notes"), Some(&false));
    assert_eq!(d.get("top_secret"), Some(&false));
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_bypasses_field_access_entirely() {
    let items = item_service_with_ref_plugin();
    // Admin: every field visible regardless of the plugin's rules (no dispatch).
    let d = items
        .field_access_decisions(
            &UserContext::authenticated(uuid::Uuid::now_v7(), vec!["administer site".to_string()]),
            "person",
            &fields(&["ssn", "salary"]),
            "view",
        )
        .await;
    assert_eq!(d.get("ssn"), Some(&true));
    assert_eq!(d.get("salary"), Some(&true));
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_type_is_fully_visible_fail_open() {
    let items = item_service_with_ref_plugin();
    // The plugin has no rules for "article" ⇒ NoOpinion ⇒ kernel fail-open.
    let d = items
        .field_access_decisions(&user(&[]), "article", &fields(&["title", "body"]), "view")
        .await;
    assert_eq!(d.get("title"), Some(&true));
    assert_eq!(d.get("body"), Some(&true));
}

#[tokio::test(flavor = "multi_thread")]
async fn accessible_fields_drops_denied_through_the_seam() {
    let items = item_service_with_ref_plugin();
    // The batch seam returns only the visible fields, in input order.
    let visible = items
        .accessible_fields(
            &user(&["view salary"]),
            "person",
            &fields(&["ssn", "salary", "bio"]),
            "view",
        )
        .await;
    assert_eq!(visible, vec!["salary".to_string(), "bio".to_string()]);
}
