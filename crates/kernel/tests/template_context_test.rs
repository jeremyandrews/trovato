#![allow(clippy::unwrap_used, clippy::expect_used)]
//! What templates are given: the viewer, plugin output on its own, and a text
//! field's format key kept out of the field dump.
//!
//! Three gaps, each observed on a served page:
//!
//! - **No viewer.** The page template got `user_authenticated` and nothing that
//!   names the viewer, and the item template got no viewer at all, so no theme
//!   could link to `/user/{uid}/...` or draw a per-viewer control.
//! - **Plugin output fused into the field dump.** `tap_item_view` output reached
//!   the item template only appended to `children`, so a theme that lays out its
//!   own fields had to print every field twice or drop every plugin's output.
//! - **B63.** A text field stored beside a `<name>_format` key printed that key
//!   as a field: "body format: filtered_html" on the page.
//!
//! # Why this file has its own app
//!
//! The stock theme prints none of this, so the file overlays
//! `crates/kernel/tests/fixtures/template-context/` on the real template
//! directory: a page layout and two item templates that print exactly the keys
//! under test. It also installs `trovato_series`, a real plugin whose
//! `tap_item_view` decorates blog posts, so the plugin output is the real wire
//! output and not a stand-in.
//!
//! Requires Postgres + Redis and `plugins/trovato_series/trovato_series.wasm`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// The probe type whose fixture template prints the viewer.
const PROBE_TYPE: &str = "b74_viewer_probe";

/// The plugin whose `tap_item_view` supplies real plugin output.
const SERIES_PLUGIN: &str = "trovato_series";

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("template context fixture app init thread panicked")
    })
}

async fn build_app() -> TestApp {
    trovato_test_utils::env::load_dotenv();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("connect for fixture setup");
    // Installed before the app is built: `AppState` resolves the enabled plugin
    // set once, at construction.
    trovato_kernel::plugin::status::install_plugin(&pool, SERIES_PLUGIN, "0.99.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{SERIES_PLUGIN}': {e:#}"));
    pool.close().await;

    let project_root = common::project_root();
    TestApp::with_config(|config| {
        // The fixture directory last, so its templates win over the stock ones.
        config.templates_dirs = vec![
            project_root.join("templates"),
            project_root.join("crates/kernel/tests/fixtures/template-context"),
        ];
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![project_root.join("plugins")];
        }
    })
    .await
}

fn admin_context() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

async fn ensure_probe_type(app: &TestApp) {
    use trovato_sdk::types::{FieldDefinition, FieldType};

    let fields = vec![
        FieldDefinition::new("field_note", FieldType::Text { max_length: None }).label("Note"),
    ];
    let settings = serde_json::json!({ "fields": serde_json::to_value(&fields).unwrap() });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'Viewer Probe', '', true, 'Title', 'b74_test', $2) \
         ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
    )
    .bind(PROBE_TYPE)
    .bind(&settings)
    .execute(&app.db)
    .await
    .expect("seed viewer probe type");
    app.state
        .content_types()
        .replace(PROBE_TYPE, "Viewer Probe", None, settings)
        .await
        .ok();
}

async fn make_item(app: &TestApp, item_type: &str, title: &str, fields: serde_json::Value) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: item_type.to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(fields),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("template context test".to_string()),
            },
            &admin_context(),
        )
        .await
        .expect("create item")
        .id;
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

async fn page(app: &TestApp, path: &str, cookies: &str) -> String {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    assert_eq!(response.status(), StatusCode::OK, "GET {path}");
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// The text of the first `<tag id="id">…</tag>` on the page.
fn element_text<'a>(html: &'a str, tag: &str, id: &str) -> Option<&'a str> {
    let open = format!("<{tag} id=\"{id}\">");
    let start = html.find(&open)? + open.len();
    let end = html[start..].find(&format!("</{tag}>"))? + start;
    Some(&html[start..end])
}

/// The text between `<!--name-->` and `<!--/name-->`.
fn between_markers<'a>(html: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<!--{name}-->");
    let start = html.find(&open)? + open.len();
    let end = html[start..].find(&format!("<!--/{name}-->"))? + start;
    Some(&html[start..end])
}

/// Undo Tera's HTML escaping of a `json_encode` result.
fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#x2F;", "/")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The `viewer` a template printed into `<pre id="{id}">`, parsed.
fn printed_viewer(html: &str, id: &str) -> serde_json::Value {
    let text = element_text(html, "pre", id)
        .unwrap_or_else(|| panic!("no #{id} on the page; is `viewer` in the context?\n{html}"));
    serde_json::from_str(&unescape(text))
        .unwrap_or_else(|e| panic!("#{id} is not JSON ({e}): {text}"))
}

/// Assert the same viewer on the page layout and in the item template.
fn assert_viewer(html: &str, id: Option<Uuid>, authenticated: bool, roles: &[&str]) {
    let expected = serde_json::json!({
        "id": id.map(|id| id.to_string()),
        "authenticated": authenticated,
        "roles": roles,
    });
    for element in ["page-viewer", "item-viewer"] {
        assert_eq!(printed_viewer(html, element), expected, "#{element}");
    }
}

#[test]
fn an_anonymous_visitor_is_named_in_the_page_and_item_templates() {
    run_test(async {
        let app = app();
        ensure_probe_type(app).await;
        let id = make_item(
            app,
            PROBE_TYPE,
            "Viewer probe (anonymous)",
            serde_json::json!({}),
        )
        .await;

        let html = page(app, &format!("/item/{id}"), "").await;
        assert_viewer(&html, None, false, &["anonymous user"]);
        assert!(!html.contains("class=\"own-page\""), "{html}");
    });
}

#[test]
fn a_member_with_an_extra_role_is_named_with_their_id_and_roles() {
    run_test(async {
        let app = app();
        ensure_probe_type(app).await;
        let id = make_item(
            app,
            PROBE_TYPE,
            "Viewer probe (member)",
            serde_json::json!({}),
        )
        .await;

        let name = common::username("b74member");
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let uid = common::user_id_of(app, &name).await;
        let role = format!("b74 reviewer {}", Uuid::now_v7().simple());
        let role_id = common::create_test_role(app, &role).await;
        trovato_kernel::models::Role::assign_to_user(&app.db, uid, role_id)
            .await
            .expect("assign role");
        app.state.permissions().invalidate_user(uid);
        let cookies = app.login(&name, "test-password-123").await;

        let html = page(app, &format!("/item/{id}"), &cookies).await;
        // Sorted: "authenticated user" before "b74 reviewer …".
        assert_viewer(&html, Some(uid), true, &["authenticated user", &role]);
        assert!(
            html.contains(&format!("<a class=\"own-page\" href=\"/user/{uid}\">")),
            "the item template can link to the viewer's own page: {html}"
        );
    });
}

#[test]
fn an_administrator_is_named_with_their_id_and_roles() {
    run_test(async {
        let app = app();
        ensure_probe_type(app).await;
        let id = make_item(
            app,
            PROBE_TYPE,
            "Viewer probe (admin)",
            serde_json::json!({}),
        )
        .await;

        let name = common::username("b74admin");
        let cookies = app
            .create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let uid = common::user_id_of(app, &name).await;

        let html = page(app, &format!("/item/{id}"), &cookies).await;
        assert_viewer(&html, Some(uid), true, &["authenticated user"]);
    });
}

#[test]
fn the_viewer_carries_exactly_id_authenticated_and_roles() {
    run_test(async {
        let app = app();
        ensure_probe_type(app).await;
        let id = make_item(
            app,
            PROBE_TYPE,
            "Viewer probe (keys)",
            serde_json::json!({}),
        )
        .await;

        let name = common::username("b74keys");
        let cookies = app
            .create_and_login_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;

        let html = page(app, &format!("/item/{id}"), &cookies).await;
        for element in ["page-viewer", "item-viewer"] {
            let viewer = printed_viewer(&html, element);
            let mut keys: Vec<&str> = viewer
                .as_object()
                .expect("viewer is an object")
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, ["authenticated", "id", "roles"], "#{element}");
        }
        // Nothing credential-adjacent anywhere in what was printed.
        let printed = format!(
            "{}{}",
            element_text(&html, "pre", "page-viewer").unwrap(),
            element_text(&html, "pre", "item-viewer").unwrap()
        );
        assert!(!printed.contains("example.com"), "{printed}");
        assert!(!printed.contains(&name), "{printed}");
    });
}

#[test]
fn a_role_name_with_markup_renders_escaped() {
    run_test(async {
        let app = app();
        ensure_probe_type(app).await;
        let id = make_item(
            app,
            PROBE_TYPE,
            "Viewer probe (escape)",
            serde_json::json!({}),
        )
        .await;

        let name = common::username("b74xss");
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let uid = common::user_id_of(app, &name).await;
        let role = format!("<script>alert({})</script>", Uuid::now_v7().simple());
        let role_id = common::create_test_role(app, &role).await;
        trovato_kernel::models::Role::assign_to_user(&app.db, uid, role_id)
            .await
            .expect("assign role");
        app.state.permissions().invalidate_user(uid);
        let cookies = app.login(&name, "test-password-123").await;

        let html = page(app, &format!("/item/{id}"), &cookies).await;
        let roles = element_text(&html, "ul", "item-roles").expect("roles list");
        assert!(roles.contains("&lt;script&gt;alert("), "{roles}");
        assert!(!html.contains("<script>alert("), "{html}");
    });
}

#[test]
fn plugin_output_reaches_the_item_template_on_its_own_and_children_is_unchanged() {
    run_test(async {
        let app = app();
        let series = format!("B74 series {}", Uuid::now_v7().simple());
        let fields =
            serde_json::json!({ "field_body": "B74 part text", "field_series_title": series });
        let first = make_item(app, "blog", "B74 part one", fields.clone()).await;
        let _second = make_item(app, "blog", "B74 part two", fields).await;

        let html = page(app, &format!("/item/{first}"), "").await;
        let plugin_output = between_markers(&html, "plugin-output")
            .unwrap_or_else(|| panic!("no `plugin_output` in the item template: {html}"));
        assert!(
            plugin_output.contains("class=\"series-nav\""),
            "the series plugin's output: {plugin_output}"
        );
        assert!(
            !plugin_output.contains("field-field_series_title"),
            "no field dump in the plugin output: {plugin_output}"
        );

        // `children` is what it always was: the field dump, then the plugin
        // output appended to it, byte for byte.
        let children = between_markers(&html, "children").expect("children");
        let dump = children
            .strip_suffix(plugin_output)
            .unwrap_or_else(|| panic!("children does not end with the plugin output:\n{children}"));
        assert!(
            dump.contains("<div class=\"field field-field_series_title\">"),
            "{dump}"
        );
        assert!(!dump.contains("series-nav"), "{dump}");
    });
}

#[test]
fn a_text_fields_format_key_is_not_printed_as_a_field() {
    run_test(async {
        let app = app();
        let id = make_item(
            app,
            "blog",
            "B74 format key",
            serde_json::json!({
                "field_body": "B74 body text",
                "field_body_format": "filtered_html",
            }),
        )
        .await;

        let html = page(app, &format!("/item/{id}"), "").await;
        let children = between_markers(&html, "children").expect("children");
        assert!(children.contains("B74 body text"), "{children}");
        assert!(!children.contains("body format"), "{children}");
        assert!(!children.contains("filtered_html"), "{children}");
        assert!(!children.contains("field-field_body_format"), "{children}");
    });
}

#[test]
fn a_field_named_like_a_format_key_still_renders() {
    run_test(async {
        let app = app();
        // `blog` declares no text field `field_layout`, so `field_layout_format`
        // is a field in its own right.
        let id = make_item(
            app,
            "blog",
            "B74 real format field",
            serde_json::json!({
                "field_body": "B74 other body",
                "field_layout_format": "landscape",
            }),
        )
        .await;

        let html = page(app, &format!("/item/{id}"), "").await;
        let children = between_markers(&html, "children").expect("children");
        assert!(
            children.contains("<div class=\"field field-field_layout_format\">"),
            "{children}"
        );
        assert!(children.contains("landscape"), "{children}");
    });
}
