#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A plugin-declared content type keeps its fields across a database round trip.
//!
//! Two shapes were written to `item_type.settings`. The core seed migration and
//! every admin-side writer stored `{"fields": [...]}`; plugin registration
//! stored the bare array `[...]`. The reader only ever understood the object
//! shape and returned an empty `Vec` for anything else, without a word.
//!
//! So `page`, seeded by the migration, rendered its body field, while `blog`
//! and every other plugin-declared type rendered a title and a Published
//! checkbox and nothing else. The same empty list reached
//! `validate_required_fields`, so an item with a required field left empty
//! saved cleanly.
//!
//! These drive the **real** writer (`sync_from_plugins`, against the real
//! `trovato_blog` wasm), the **real** reader (`reload_from_db`), and the real
//! admin routes over HTTP.
//!
//! Build the wasm first:
//!
//! ```text
//! cargo build -p trovato_blog --target wasm32-wasip1 --release \
//!   && cp target/wasm32-wasip1/release/trovato_blog.wasm plugins/trovato_blog/
//! ```

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app};

use trovato_kernel::content::ContentTypeRegistry;
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::tap::{TapDispatcher, TapRegistry};

/// The plugin under test declares `blog` with a required `field_body` and a
/// multi-valued `field_tags`.
const PLUGIN: &str = "trovato_blog";
const TYPE: &str = "blog";

static DISPATCHER: OnceLock<Arc<TapDispatcher>> = OnceLock::new();

fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

fn dispatcher() -> Arc<TapDispatcher> {
    DISPATCHER
        .get_or_init(|| {
            let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
            runtime
                .load_plugin(&plugins_dir().join(PLUGIN))
                .unwrap_or_else(|e| {
                    panic!(
                        "failed to load '{PLUGIN}': {e:#}\n\
                         build it: cargo build -p {PLUGIN} --target wasm32-wasip1 --release \
                         && cp target/wasm32-wasip1/release/{PLUGIN}.wasm plugins/{PLUGIN}/"
                    )
                });
            let runtime = Arc::new(runtime);
            let registry = Arc::new(TapRegistry::from_plugins(&runtime));
            Arc::new(TapDispatcher::new(runtime, registry))
        })
        .clone()
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// Register `blog` exactly as startup does, through the real writer.
async fn register_through_the_real_writer(app: &TestApp) {
    ContentTypeRegistry::new(app.db.clone(), Duration::from_secs(60))
        .sync_from_plugins(&dispatcher())
        .await
        .expect("sync content types from plugins");
}

/// Pull a hidden input's value out of rendered form HTML.
fn input_value(html: &str, name: &str) -> Option<String> {
    let needle = format!(r#"name="{name}""#);
    let at = html.find(&needle)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(tag[vat..vend].to_string())
}

/// **The finding, at the writer.** Plugin registration stores the canonical
/// object shape, so the reader gets the fields back.
#[test]
fn plugin_registration_writes_the_canonical_settings_shape() {
    run_test(async {
        let app = shared_app().await;
        register_through_the_real_writer(app).await;

        let settings: serde_json::Value =
            sqlx::query_scalar("SELECT settings FROM item_type WHERE type = $1")
                .bind(TYPE)
                .fetch_one(&app.db)
                .await
                .expect("the sync must have written a 'blog' row");

        assert!(
            settings.is_object(),
            "item_type.settings must be the canonical object shape, got {}: {settings}",
            if settings.is_array() {
                "the bare array the reader cannot read"
            } else {
                "neither an object nor an array"
            }
        );
        let fields = settings
            .get("fields")
            .and_then(|v| v.as_array())
            .expect("the object must carry a 'fields' array");
        assert_eq!(fields.len(), 2, "blog declares field_body and field_tags");
    });
}

/// **The finding, at the reader.** Loading from the database keeps the fields
/// instead of silently flattening them to none.
#[test]
fn a_database_round_trip_keeps_the_declared_fields() {
    run_test(async {
        let app = shared_app().await;
        register_through_the_real_writer(app).await;

        // Drop anything the cache is holding, so this reads the database.
        app.state.content_types().invalidate(TYPE);
        app.state
            .content_types()
            .reload_from_db()
            .await
            .expect("reload content types from the database");

        let def = app
            .state
            .content_types()
            .get(TYPE)
            .expect("'blog' must be in the registry");
        let names: Vec<&str> = def.fields.iter().map(|f| f.field_name.as_str()).collect();
        assert!(
            names.contains(&"field_body") && names.contains(&"field_tags"),
            "a round trip through the database dropped blog's fields: {names:?}"
        );
        assert!(
            def.fields
                .iter()
                .any(|f| f.field_name == "field_body" && f.required),
            "field_body must come back still marked required"
        );
    });
}

/// **The visible symptom.** The admin add form renders the type's declared
/// field inputs, not just a title and a Published checkbox.
#[test]
fn the_admin_add_form_renders_the_declared_fields() {
    run_test(async {
        let app = shared_app().await;
        register_through_the_real_writer(app).await;
        app.state.content_types().invalidate(TYPE);
        app.state
            .content_types()
            .reload_from_db()
            .await
            .expect("reload content types");
        app.ensure_plugin_enabled(PLUGIN).await;

        let cookies = app
            .create_and_login_admin(
                "ctshapeform",
                "correct-horse-battery-staple",
                "ctshapeform@test.local",
            )
            .await;

        let page = app
            .request_with_cookies(
                Request::builder()
                    .uri(format!("/admin/content/add/{TYPE}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let html = body_string(page).await;

        assert!(
            html.contains(r#"name="field_body""#),
            "the add form rendered none of blog's declared fields"
        );
        assert!(
            html.contains(r#"name="field_tags""#),
            "the add form is missing field_tags"
        );
    });
}

/// **The other half of the same defect.** An empty field list disabled
/// validation, so a required field could be left empty. It cannot now.
#[test]
fn a_save_missing_a_required_field_is_refused() {
    run_test(async {
        let app = shared_app().await;
        register_through_the_real_writer(app).await;
        app.state.content_types().invalidate(TYPE);
        app.state
            .content_types()
            .reload_from_db()
            .await
            .expect("reload content types");
        app.ensure_plugin_enabled(PLUGIN).await;

        let cookies = app
            .create_and_login_admin(
                "ctshapereq",
                "correct-horse-battery-staple",
                "ctshapereq@test.local",
            )
            .await;

        // This binary shares a database with every other test target, and with
        // its own earlier runs. Clear the row this case asserts the absence of.
        sqlx::query("DELETE FROM item WHERE title = 'A blog post with no body'")
            .execute(&app.db)
            .await
            .unwrap();

        // Render the form to get a CSRF token a browser would carry.
        let page = app
            .request_with_cookies(
                Request::builder()
                    .uri(format!("/admin/content/add/{TYPE}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let html = body_string(page).await;
        let csrf = input_value(&html, "_token").expect("the form must carry a CSRF token");
        let build_id =
            input_value(&html, "_form_build_id").expect("the form must carry a build id");

        // Post a title and no field_body, which is required. The title is
        // spelled with `+` for its spaces, as a browser would send it.
        let form_body = format!(
            "_token={csrf}&_form_build_id={build_id}&title=A+blog+post+with+no+body&status=1"
        );
        let response = app
            .request_with_cookies(
                Request::builder()
                    .method("POST")
                    .uri(format!("/admin/content/add/{TYPE}"))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(form_body))
                    .unwrap(),
                &cookies,
            )
            .await;

        let status = response.status();
        let body = body_string(response).await;
        assert!(
            body.contains("Body is required"),
            "a required field left empty must be refused; got {status} and a page that did not \
             mention it"
        );

        let saved: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM item WHERE title = 'A blog post with no body'",
        )
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(saved, 0, "the refused item must not have been written");
    });
}

/// The shipped migration repairs a row already written in the old shape.
///
/// This runs the migration's own SQL text, read from the file, against a
/// seeded legacy row — so it tests what ships rather than a copy of it. The
/// row has a name no other test uses, and is cleaned up either side, so it
/// does not depend on what else has touched this shared database.
#[test]
fn the_migration_repairs_a_legacy_array_shaped_row() {
    run_test(async {
        let app = shared_app().await;
        const SCRATCH: &str = "ct_shape_legacy";

        let migration = std::fs::read_to_string(
            common::project_root()
                .join("crates/kernel/migrations/20260925000001_normalize_item_type_settings.sql"),
        )
        .expect("read the normalization migration");

        let legacy = serde_json::json!([{
            "field_name": "field_legacy",
            "field_type": "TextLong",
            "label": "Legacy",
            "required": true,
            "cardinality": 1,
            "settings": {},
            "personal_data": false,
        }]);

        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, 'Legacy Shape', '', true, 'Title', 'test', $2) \
             ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
        )
        .bind(SCRATCH)
        .bind(&legacy)
        .execute(&app.db)
        .await
        .expect("seed a row in the legacy array shape");

        sqlx::raw_sql(&migration)
            .execute(&app.db)
            .await
            .expect("apply the normalization migration");

        let settings: serde_json::Value =
            sqlx::query_scalar("SELECT settings FROM item_type WHERE type = $1")
                .bind(SCRATCH)
                .fetch_one(&app.db)
                .await
                .unwrap();

        sqlx::query("DELETE FROM item_type WHERE type = $1")
            .bind(SCRATCH)
            .execute(&app.db)
            .await
            .unwrap();

        assert_eq!(
            settings,
            serde_json::json!({ "fields": legacy }),
            "the migration must lift a bare array into the canonical object shape"
        );

        // And the field definitions survive the lift intact.
        let fields: Vec<trovato_sdk::types::FieldDefinition> =
            serde_json::from_value(settings.get("fields").unwrap().clone())
                .expect("the migrated fields must still deserialize");
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_name, "field_legacy");
        assert!(fields[0].required);
    });
}
