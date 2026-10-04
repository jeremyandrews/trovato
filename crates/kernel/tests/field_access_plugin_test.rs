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
use trovato_kernel::content::{ItemService, WriteDenied};
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
            &UserContext::administrator(uuid::Uuid::now_v7(), vec!["administer site".to_string()]),
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

// ===========================================================================
// S4 — the write side: the same decision, asked for `"edit"`, on the way in.
//
// Field access was built and tested for reading. `field_access_decisions` has
// always taken an `operation` and the design has always named two, but every
// caller asked for `"view"` and no write path asked at all — so a user who
// could edit an item could overwrite a field they were not allowed to see.
//
// `ItemService::gate_field_writes` is the gate `create`, `update`,
// `revert_to_revision` and `save_translation` all run. It needs no database:
// it decides from the submitted and stored `fields` objects and the plugin's
// answer, which is why it can be driven here through the real reference plugin
// on the same never-connected pool as the tests above.
//
// The reference plugin decides on permissions and ignores `operation`, so with
// it enabled an edit decision equals a view decision. That is what makes these
// assertions readable: `ssn` needs `view pii` either way.
// ===========================================================================

/// A `fields` object of flat string values, the shape the forms write.
fn obj(pairs: &[(&str, &str)]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in pairs {
        map.insert(
            (*k).to_string(),
            serde_json::Value::String((*v).to_string()),
        );
    }
    serde_json::Value::Object(map)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_edit_operation_is_asked_for_and_answered() {
    let items = item_service_with_ref_plugin();
    // The first assertion in this file on `"edit"`: before S4 nothing but a
    // unit test ever passed that operation, which is the defect stated as a
    // test.
    let d = items
        .field_access_decisions(
            &user(&["view salary"]),
            "person",
            &fields(&["ssn", "salary", "bio"]),
            "edit",
        )
        .await;
    assert_eq!(
        d.get("ssn"),
        Some(&false),
        "no 'view pii' ⇒ ssn not editable"
    );
    assert_eq!(d.get("salary"), Some(&true), "'view salary' ⇒ editable");
    assert_eq!(d.get("bio"), Some(&true), "ungoverned ⇒ fail-open editable");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_submitting_a_denied_field_is_refused() {
    let items = item_service_with_ref_plugin();
    let err = items
        .gate_field_writes(
            &user(&["create person content"]),
            "person",
            Some(&obj(&[("bio", "hi"), ("ssn", "123-45-6789")])),
            None,
        )
        .await
        .expect_err("writing ssn without 'view pii' must be refused");
    assert_eq!(err, WriteDenied::Fields(vec!["ssn".to_string()]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_without_the_denied_field_is_accepted() {
    let items = item_service_with_ref_plugin();
    let out = items
        .gate_field_writes(
            &user(&["create person content"]),
            "person",
            Some(&obj(&[("bio", "hi")])),
            None,
        )
        .await
        .expect("a create touching no denied field is fine");
    assert_eq!(out, Some(obj(&[("bio", "hi")])));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_leaving_a_denied_field_out_copies_it_back() {
    let items = item_service_with_ref_plugin();
    // A JSON update replaces the whole `fields` object, so without the copy-back
    // a partial submission would erase the field it was never allowed to see.
    let out = items
        .gate_field_writes(
            &user(&["edit any content"]),
            "person",
            Some(&obj(&[("bio", "edited")])),
            Some(&obj(&[("bio", "old"), ("ssn", "123-45-6789")])),
        )
        .await
        .expect("leaving a denied field out is not a change")
        .expect("fields were submitted");
    assert_eq!(out, obj(&[("bio", "edited"), ("ssn", "123-45-6789")]));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_resubmitting_the_denied_value_unchanged_is_accepted() {
    let items = item_service_with_ref_plugin();
    let stored = obj(&[("bio", "old"), ("ssn", "123-45-6789")]);
    let out = items
        .gate_field_writes(
            &user(&["edit any content"]),
            "person",
            Some(&obj(&[("bio", "edited"), ("ssn", "123-45-6789")])),
            Some(&stored),
        )
        .await
        .expect("an identical value is not a change");
    assert_eq!(out, Some(obj(&[("bio", "edited"), ("ssn", "123-45-6789")])));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_changing_a_denied_field_is_refused() {
    let items = item_service_with_ref_plugin();
    let err = items
        .gate_field_writes(
            &user(&["edit any content"]),
            "person",
            Some(&obj(&[("ssn", "999-99-9999")])),
            Some(&obj(&[("ssn", "123-45-6789")])),
        )
        .await
        .expect_err("overwriting a field the user may not see must be refused");
    assert_eq!(err, WriteDenied::Fields(vec!["ssn".to_string()]));
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_denied_field_is_a_change_and_is_refused() {
    let items = item_service_with_ref_plugin();
    // Explicit `null`, as opposed to leaving the key out: a deletion is a write.
    let mut submitted = serde_json::Map::new();
    submitted.insert("ssn".to_string(), serde_json::Value::Null);
    let err = items
        .gate_field_writes(
            &user(&["edit any content"]),
            "person",
            Some(&serde_json::Value::Object(submitted)),
            Some(&obj(&[("ssn", "123-45-6789")])),
        )
        .await
        .expect_err("nulling a denied field must be refused");
    assert_eq!(err, WriteDenied::Fields(vec!["ssn".to_string()]));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_permission_holder_may_write_the_field() {
    let items = item_service_with_ref_plugin();
    let out = items
        .gate_field_writes(
            &user(&["edit any content", "view pii"]),
            "person",
            Some(&obj(&[("ssn", "999-99-9999")])),
            Some(&obj(&[("ssn", "123-45-6789")])),
        )
        .await
        .expect("'view pii' may write ssn");
    assert_eq!(out, Some(obj(&[("ssn", "999-99-9999")])));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_administrator_may_write_the_field() {
    let items = item_service_with_ref_plugin();
    let out = items
        .gate_field_writes(
            &UserContext::administrator(uuid::Uuid::now_v7(), vec!["administer site".to_string()]),
            "person",
            Some(&obj(&[("ssn", "999-99-9999")])),
            Some(&obj(&[("ssn", "123-45-6789")])),
        )
        .await
        .expect("the administrator bypass is the one in field_access_decisions");
    assert_eq!(out, Some(obj(&[("ssn", "999-99-9999")])));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_that_changes_no_fields_at_all_decides_nothing() {
    let items = item_service_with_ref_plugin();
    // `None` is "I am not touching fields" — a bulk status change, say — and it
    // must stay `None` so the model layer leaves the stored object alone.
    let out = items
        .gate_field_writes(
            &user(&["edit any content"]),
            "person",
            None,
            Some(&obj(&[("ssn", "123-45-6789")])),
        )
        .await
        .expect("no submitted fields, nothing to refuse");
    assert_eq!(out, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_background_principal_writes_every_field() {
    let items = item_service_with_ref_plugin();
    // Cron and the queue worker write as nobody — no identity, no permissions
    // — so every governed field would be denied to them and a scheduled job
    // re-saving an item would fail. The marker is constructed by no web,
    // session or auth path, so this is not a channel a user can reach.
    let out = items
        .gate_field_writes(
            &UserContext::background(),
            "person",
            Some(&obj(&[("ssn", "999-99-9999")])),
            Some(&obj(&[("ssn", "123-45-6789")])),
        )
        .await
        .expect("the background principal is not held to a human permission");
    assert_eq!(out, Some(obj(&[("ssn", "999-99-9999")])));
}
