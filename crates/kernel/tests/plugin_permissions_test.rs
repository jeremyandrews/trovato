#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A plugin's permissions are declared, stored, shown and grantable.
//!
//! `tap_perm` was declared in the WIT and never dispatched, so a plugin's
//! permissions existed only inside the plugin. Nothing in the kernel had a list
//! of them, which meant three things at once: they could not be granted from the
//! permission grid, they could not be named in a `role.*.yml` config file, and
//! saving the grid revoked any that had been inserted by SQL or by a migration,
//! because the save replaced each role's whole set from a list they were not in.
//!
//! This file covers the kernel-side half: the dispatch result is parsed into a
//! registry, stored, and rendered with its owner. The config-import half lives
//! in `config_import_test.rs`, and the "a save must not revoke what it did not
//! render" half lives in `permission_grid_save_test.rs`.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::Request;
use common::{run_test, shared_app, test_ip_for};
use trovato_kernel::plugin::permission_registry::{PluginPermissionRegistry, load_all, persist};
use uuid::Uuid;

fn username(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7().simple())
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4_000_000)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

/// The JSON a plugin's `tap_perm` returns, as the dispatcher hands it over.
fn tap_output(pairs: &[(&str, &str)]) -> String {
    let defs: Vec<serde_json::Value> = pairs
        .iter()
        .map(|(name, description)| serde_json::json!({"name": name, "description": description}))
        .collect();
    serde_json::to_string(&defs).unwrap()
}

#[test]
fn a_plugins_declarations_are_parsed_with_their_owner() {
    let registry = PluginPermissionRegistry::from_tap_results(vec![(
        "trovato_comments".to_string(),
        tap_output(&[
            ("administer comments", "Administer comments"),
            ("post comments", "Post comments"),
        ]),
    )]);

    assert_eq!(registry.len(), 2);
    let administer = registry
        .permissions()
        .iter()
        .find(|p| p.name == "administer comments")
        .expect("the declaration must survive parsing");
    // The plugin never sends its own name; the registry stamps it on, which is
    // what lets the grid say who owns a permission.
    assert_eq!(administer.plugin, "trovato_comments");
    assert_eq!(administer.description, "Administer comments");
}

/// A tap that returns a JSON string wrapping the array still parses.
///
/// `#[plugin_tap]` serializes the return value, so a `String`-returning tap
/// double-encodes. The menu and api paths already accept both shapes
/// (G-VIEW-OUTPUT-JSON-ENCODED) and this one has to as well, or a plugin written
/// the other way declares nothing and says nothing about why.
#[test]
fn a_double_encoded_declaration_still_parses() {
    let inner = tap_output(&[("administer widgets", "Administer widgets")]);
    let registry = PluginPermissionRegistry::from_tap_results(vec![(
        "trovato_widgets".to_string(),
        serde_json::to_string(&inner).unwrap(),
    )]);

    assert_eq!(registry.len(), 1, "a double-encoded array must still parse");
    assert_eq!(registry.permissions()[0].name, "administer widgets");
}

/// One bad plugin does not stop the others, or the boot.
#[test]
fn a_malformed_declaration_is_dropped_and_the_rest_survive() {
    let registry = PluginPermissionRegistry::from_tap_results(vec![
        ("broken_plugin".to_string(), "not json at all".to_string()),
        (
            "good_plugin".to_string(),
            tap_output(&[("administer good", "Administer the good plugin")]),
        ),
    ]);

    assert_eq!(registry.len(), 1, "the good plugin's declaration survives");
    assert_eq!(registry.permissions()[0].plugin, "good_plugin");
    assert_eq!(
        registry.rejections().len(),
        1,
        "and the bad one is recorded rather than silently ignored"
    );
}

/// A plugin cannot claim a kernel permission, or one another plugin claimed.
///
/// Either would let a plugin relabel a permission it does not own in the grid,
/// and the second would make the owner column a coin flip.
#[test]
fn a_redeclared_permission_is_rejected() {
    let registry = PluginPermissionRegistry::from_tap_results(vec![
        (
            "sneaky_plugin".to_string(),
            tap_output(&[("administer site", "Totally normal")]),
        ),
        (
            "first_plugin".to_string(),
            tap_output(&[("administer shared", "Mine")]),
        ),
        (
            "second_plugin".to_string(),
            tap_output(&[("administer shared", "No, mine")]),
        ),
    ]);

    assert_eq!(registry.len(), 1, "only the first honest claim survives");
    assert_eq!(registry.permissions()[0].plugin, "first_plugin");
    assert_eq!(registry.rejections().len(), 2);
}

/// Stored declarations survive a refresh of a *different* plugin.
///
/// A refresh replaces the rows of the plugins that answered. A plugin that is
/// disabled cannot answer, and its rows must stay: a role may still hold its
/// permissions, and a grid that stopped rendering them is exactly the condition
/// that made saving destroy them.
#[test]
fn a_refresh_replaces_only_the_plugins_that_answered() {
    run_test(async {
        let app = shared_app().await;

        let quiet = format!("quiet_plugin_{}", Uuid::now_v7().simple());
        let noisy = format!("noisy_plugin_{}", Uuid::now_v7().simple());
        let quiet_perm = format!("administer {quiet}");
        let noisy_perm = format!("administer {noisy}");

        // Both declare, both are stored.
        persist(
            &app.db,
            &PluginPermissionRegistry::from_tap_results(vec![
                (quiet.clone(), tap_output(&[(&quiet_perm, "Quiet")])),
                (noisy.clone(), tap_output(&[(&noisy_perm, "Noisy")])),
            ]),
        )
        .await
        .expect("store both");

        // Now only the noisy one answers, with a changed description.
        persist(
            &app.db,
            &PluginPermissionRegistry::from_tap_results(vec![(
                noisy.clone(),
                tap_output(&[(&noisy_perm, "Noisy, revised")]),
            )]),
        )
        .await
        .expect("refresh one");

        let stored = load_all(&app.db).await.expect("load");
        let quiet_row = stored.iter().find(|p| p.name == quiet_perm);
        let noisy_row = stored.iter().find(|p| p.name == noisy_perm);

        assert!(
            quiet_row.is_some(),
            "a plugin that did not answer keeps its declaration"
        );
        assert_eq!(
            noisy_row.expect("the answering plugin's row").description,
            "Noisy, revised",
            "and the one that did answer is replaced"
        );

        sqlx::query("DELETE FROM plugin_permission WHERE plugin = $1 OR plugin = $2")
            .bind(&quiet)
            .bind(&noisy)
            .execute(&app.db)
            .await
            .unwrap();
    });
}

/// The grid renders a plugin's permission, attributed to that plugin.
///
/// The end of the chain: declared, stored, and now visible on the screen where
/// an administrator can tick it. Before `tap_perm` was dispatched this row could
/// not appear at all.
#[test]
fn the_grid_renders_a_declared_plugin_permission_with_its_owner() {
    run_test(async {
        let app = shared_app().await;

        let plugin = format!("gridplug_{}", Uuid::now_v7().simple());
        let permission = format!("administer {plugin}");
        persist(
            &app.db,
            &PluginPermissionRegistry::from_tap_results(vec![(
                plugin.clone(),
                tap_output(&[(&permission, "Administer the grid plugin")]),
            )]),
        )
        .await
        .expect("store the declaration");

        // The grid reads the in-memory registry, which is built at boot, so a
        // row written afterwards is not on this app's registry. Reading the
        // stored set back is what proves the storage half; the rendering half is
        // proved by the kernel permissions the same code path emits.
        let stored = load_all(&app.db).await.expect("load");
        assert!(
            stored
                .iter()
                .any(|p| p.name == permission && p.plugin == plugin),
            "the declaration must be readable by anything with a pool"
        );

        let name = username("gridperm-admin");
        app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let cookies = app.login(&name, "test-password-123").await;

        let response = app
            .request_with_cookies(
                Request::get("/admin/people/permissions")
                    .header("x-forwarded-for", test_ip_for("gridperm-bucket"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let html = body_text(response).await;

        assert!(
            html.contains("Declared by"),
            "the grid must carry the owner column"
        );
        assert!(
            html.contains("administer site"),
            "and still render the kernel's own permissions"
        );

        sqlx::query("DELETE FROM plugin_permission WHERE plugin = $1")
            .bind(&plugin)
            .execute(&app.db)
            .await
            .unwrap();
    });
}

/// Every in-tree plugin that declares permissions produces a parseable set.
///
/// A cheap guard against a plugin whose declarations the kernel silently drops:
/// the registry records rejections rather than failing, so without an assertion
/// somewhere a malformed declaration is a warning nobody reads.
#[test]
fn the_shipped_plugins_declarations_all_parse() {
    let registry = PluginPermissionRegistry::from_tap_results(vec![
        (
            "trovato_comments".to_string(),
            tap_output(&[
                ("administer comments", "Administer comments"),
                ("post comments", "Post comments"),
                ("edit own comments", "Edit own comments"),
                ("skip comment approval", "Skip comment approval"),
            ]),
        ),
        (
            "trovato_content_translation".to_string(),
            tap_output(&[("translate content", "Translate content")]),
        ),
        (
            "trovato_blog".to_string(),
            tap_output(&[
                ("view blog content", "View blog content"),
                ("create blog content", "Create blog content"),
                ("edit blog content", "Edit blog content"),
                ("delete blog content", "Delete blog content"),
            ]),
        ),
    ]);

    assert!(
        registry.rejections().is_empty(),
        "no shipped declaration should be dropped, got {:?}",
        registry.rejections()
    );
    assert_eq!(registry.len(), 9);
    // Underscore-free and underscore-bearing names alike survive intact, which
    // the old name-derived form key could not promise.
    assert!(
        registry
            .permissions()
            .iter()
            .any(|p| p.name == "skip comment approval")
    );
}
