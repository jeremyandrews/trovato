#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Every content and profile form goes through one pipeline (B71 Part 3).
//!
//! Before B71 no route called `FormService::build` or `process`: the item
//! forms, the admin content form and the profile form were hand built HTML, so
//! `tap_form_alter`, `tap_form_validate` and `tap_form_submit` never ran for
//! them. `FormService::build` also handed every plugin the same original form
//! and kept the last answer, so two alterations could not compose.
//!
//! These drive the real router with two fixture plugins,
//! `plugins/test_form_taps` (alter, validate, submit) and
//! `plugins/test_form_taps_b` (an alter that only acts on what the first one
//! added), plus `trovato_ai`, whose AI Assist buttons arrive the same way. All
//! three are `default_enabled = false`, so this file installs them and builds
//! its own `TestApp`, and puts their status back on the way out.
//!
//! Requires Postgres + Redis and the fixture `.wasm` files built into
//! `plugins/`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::TestApp;
use serde_json::{Value, json};
use uuid::Uuid;

const FIXTURES: [&str; 2] = ["test_form_taps", "test_form_taps_b"];
const AI: &str = "trovato_ai";
const PROBE: &str = "b71_alter_probe";
const BLOCKS: &str = "b71_blocks_probe";
const WHEN: &str = "b71_when_probe";
const REF_TARGET: &str = "b71_ref_target";
const REF_PROBE: &str = "b71_ref_probe";
const ALTERED: &str = "Altered by test_form_taps";
const COMPOSED: &str = "test_form_taps_b saw test_form_taps";
const REFUSED: &str = "The probe refuses this note.";

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();
/// `trovato_ai`'s status before this file enabled it, to restore.
static AI_STATUS: std::sync::OnceLock<Option<i16>> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("form pipeline fixture app init thread panicked")
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
        .expect("failed to connect for fixture setup");
    let ai_status: Option<i16> =
        sqlx::query_scalar("SELECT status FROM plugin_status WHERE name = $1")
            .bind(AI)
            .fetch_optional(&pool)
            .await
            .unwrap();
    AI_STATUS.get_or_init(|| ai_status);
    for plugin in FIXTURES.iter().chain([AI].iter()) {
        trovato_kernel::plugin::status::install_plugin(&pool, plugin, "1.0.0")
            .await
            .unwrap_or_else(|e| panic!("failed to install '{plugin}': {e:#}"));
    }
    pool.close().await;

    // The enabled plugin set is resolved when the state is built, so the
    // fixtures must be installed first. See `plugin_api_test.rs`.
    let app = TestApp::with_config(|config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
    })
    .await;
    ensure_types(&app).await;
    app
}

/// Put the plugins' status back so they do not load in other test binaries.
async fn restore_plugins(app: &TestApp) {
    sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = ANY($1)")
        .bind(FIXTURES.map(String::from).to_vec())
        .execute(&app.db)
        .await
        .ok();
    match AI_STATUS.get().copied().flatten() {
        Some(status) => {
            sqlx::query("UPDATE plugin_status SET status = $1 WHERE name = $2")
                .bind(status)
                .bind(AI)
                .execute(&app.db)
                .await
                .ok();
        }
        None => {
            sqlx::query("DELETE FROM plugin_status WHERE name = $1")
                .bind(AI)
                .execute(&app.db)
                .await
                .ok();
        }
    }
}

/// Run one test against the fixture app, restoring plugin status after.
fn run<F, Fut>(test: F)
where
    F: FnOnce(&'static TestApp) -> Fut + Send,
    Fut: std::future::Future<Output = ()> + Send,
{
    common::run_test(async {
        let app = app();
        test(app).await;
        restore_plugins(app).await;
    });
}

/// The probe type the fixture alters (a note and an unlimited tag list) and a
/// type with a Blocks field.
async fn ensure_types(app: &TestApp) {
    use trovato_sdk::types::{FieldDefinition, FieldType};

    let types = [
        (
            PROBE,
            "B71 Alter Probe",
            vec![
                FieldDefinition::new("field_note", FieldType::Text { max_length: None })
                    .label("Note"),
                FieldDefinition::new("field_tags", FieldType::Text { max_length: None })
                    .label("Tags")
                    .cardinality(-1),
            ],
        ),
        (
            BLOCKS,
            "B71 Blocks Probe",
            vec![FieldDefinition::new("field_body", FieldType::Blocks).label("Body")],
        ),
        (
            WHEN,
            "B71 Conditional Probe",
            vec![
                FieldDefinition::new("field_online", FieldType::Boolean).label("Online"),
                FieldDefinition::new("field_url", FieldType::Text { max_length: None })
                    .label("Stream URL")
                    .required()
                    .visible_when("field_online", "1"),
            ],
        ),
        (REF_TARGET, "B71 Reference Target", vec![]),
        (
            REF_PROBE,
            "B71 Reference Probe",
            vec![
                FieldDefinition::new(
                    "field_ref",
                    FieldType::RecordReference(REF_TARGET.to_string()),
                )
                .label("Related"),
            ],
        ),
    ];
    for (machine, label, fields) in types {
        let settings = json!({ "fields": serde_json::to_value(&fields).unwrap() });
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, $2, '', true, 'Title', 'b71_test', $3) \
             ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
        )
        .bind(machine)
        .bind(label)
        .bind(&settings)
        .execute(&app.db)
        .await
        .unwrap();
        app.state
            .content_types()
            .replace(machine, label, None, settings)
            .await
            .ok();
    }
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// Pull an input's value out of rendered HTML.
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

/// The token and build id a browser posts back.
fn form_tokens(html: &str) -> String {
    let token = input_value(html, "_token").expect("the form carries a CSRF token");
    let build = input_value(html, "_form_build_id").expect("the form carries a build id");
    format!(
        "_token={}&_form_build_id={}",
        urlencoding::encode(&token),
        urlencoding::encode(&build)
    )
}

/// GET a page as HTML, with Tera's escaped `/` (`&#x2F;`) read back as the
/// browser reads it, so a URL can be matched as written.
async fn get_page(app: &TestApp, cookies: &str, uri: &str) -> String {
    let page = app
        .request_with_cookies(
            Request::builder().uri(uri).body(Body::empty()).unwrap(),
            cookies,
        )
        .await;
    assert_eq!(page.status(), StatusCode::OK, "GET {uri}");
    body_string(page).await.replace("&#x2F;", "/")
}

/// The `<form …>…</form>` part of a page.
fn form_markup(html: &str) -> &str {
    let start = html.find("<form").expect("the page has a form");
    let end = html[start..].find("</form>").expect("the form is closed") + start;
    &html[start..end]
}

async fn post_form(app: &TestApp, cookies: &str, uri: &str, body: String) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn seed_item(app: &TestApp, item_type: &str, author: Uuid, fields: Value) -> Uuid {
    seed_titled(app, item_type, author, "Pipeline probe", fields, None).await
}

async fn seed_titled(
    app: &TestApp,
    item_type: &str,
    author: Uuid,
    title: &str,
    fields: Value,
    stage_id: Option<Uuid>,
) -> Uuid {
    // Seeding, not under test: the administrator may create on any stage,
    // which a permission list alone may not since B69.
    let user = trovato_kernel::tap::UserContext::administrator(author, vec![]);
    app.state
        .items()
        .create(
            trovato_kernel::models::CreateItem {
                item_type: item_type.to_string(),
                title: title.to_string(),
                author_id: author,
                status: Some(1),
                promote: None,
                sticky: None,
                fields: Some(fields),
                stage_id,
                language: None,
                log: None,
            },
            &user,
        )
        .await
        .expect("seed item")
        .id
}

async fn stored_fields(app: &TestApp, id: Uuid) -> Value {
    sqlx::query_scalar("SELECT fields FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

async fn admin(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let name = common::username(prefix);
    let cookies = app
        .create_and_login_admin(
            &name,
            "correct-horse-battery-staple",
            &format!("{name}@test.local"),
        )
        .await;
    (common::user_id_of(app, &name).await, cookies)
}

/// The three item form pages for one probe item.
fn item_forms(id: Uuid) -> [String; 4] {
    [
        format!("/item/{id}/edit"),
        format!("/admin/content/{id}/edit"),
        format!("/item/add/{PROBE}"),
        format!("/admin/content/add/{PROBE}"),
    ]
}

/// **Item 1.** A plugin's `tap_form_alter` reaches `/item/{id}/edit`, the admin
/// content form (edit and add) and the profile form.
#[test]
fn a_plugin_alter_reaches_the_item_admin_and_profile_forms() {
    run(|app| async move {
        let (uid, cookies) = admin(app, "b71alter").await;
        let id = seed_item(app, PROBE, uid, json!({"field_note": "hi"})).await;

        for path in item_forms(id) {
            let html = get_page(app, &cookies, &path).await;
            assert!(html.contains(ALTERED), "{path}: no altered element");
        }

        // The fixture alters the profile form only for holders of its
        // permission, so a user without it sees the plain form.
        let (_plain, plain_cookies) = common::user_holding(app, "b71plain", &[]).await;
        let html = get_page(app, &plain_cookies, "/user/profile").await;
        assert!(!html.contains(ALTERED), "altered without the permission");

        let (_probe, probe_cookies) =
            common::user_holding(app, "b71prof", &["b71 probe profile"]).await;
        let html = get_page(app, &probe_cookies, "/user/profile").await;
        assert!(html.contains(ALTERED), "the profile form was not altered");
        assert!(
            html.contains(r#"name="mail""#),
            "the profile form lost its fields"
        );
    });
}

/// **Item 5.** Two plugins altering one form compose: the second receives what
/// the first returned, in weight order.
#[test]
fn two_altering_plugins_compose() {
    run(|app| async move {
        let (uid, cookies) = admin(app, "b71comp").await;
        let id = seed_item(app, PROBE, uid, json!({})).await;
        for path in item_forms(id) {
            let html = get_page(app, &cookies, &path).await;
            assert!(html.contains(ALTERED), "{path}: first alter missing");
            assert!(
                html.contains(COMPOSED),
                "{path}: the second alter did not see the first"
            );
        }
    });
}

/// `tap_form_validate` refuses a save with an error on the field it names, the
/// editor sees it, and nothing is written.
#[test]
fn a_plugin_validation_error_refuses_the_save() {
    run(|app| async move {
        let (uid, cookies) = admin(app, "b71valid").await;
        for path in [
            "/item/{id}/edit".to_string(),
            "/admin/content/{id}/edit".to_string(),
        ] {
            let id = seed_item(app, PROBE, uid, json!({"field_note": "kept"})).await;
            let path = path.replace("{id}", &id.to_string());
            let html = get_page(app, &cookies, &path).await;
            let body = format!(
                "{}&title=Pipeline+probe&field_note=refuse+me",
                form_tokens(&html)
            );
            let (status, text) = post_form(app, &cookies, &path, body).await;
            // Not saved: the form comes back (422 on `/item`, 200 on the admin
            // form, as for any other form error there), not a redirect.
            assert!(!status.is_redirection(), "{path}: saved ({status})");
            assert!(text.contains(REFUSED), "{path}: the editor sees no error");
            // The error is shown on the field, not only at the top.
            assert!(
                text.contains(&format!(
                    r#"<div id="error-field_note" class="form-item__error" role="alert">{REFUSED}</div>"#
                )),
                "{path}: the error is not attached to field_note"
            );
            assert_eq!(
                stored_fields(app, id).await["field_note"],
                json!("kept"),
                "{path}: a refused save wrote"
            );
        }
    });
}

/// `tap_form_submit` fires exactly once for one successful save, after it.
#[test]
fn tap_form_submit_fires_once_per_save() {
    run(|app| async move {
        let (uid, cookies) = admin(app, "b71submit").await;
        for path in [
            "/item/{id}/edit".to_string(),
            "/admin/content/{id}/edit".to_string(),
        ] {
            let id = seed_item(app, PROBE, uid, json!({"field_note": "a"})).await;
            let path = path.replace("{id}", &id.to_string());
            let html = get_page(app, &cookies, &path).await;
            let body = format!("{}&title=Pipeline+probe&field_note=b", form_tokens(&html));
            let (status, text) = post_form(app, &cookies, &path, body).await;
            // `/item/{id}/edit` answers a save with the item as JSON and the
            // admin form with a redirect, as they did before B71.
            assert!(
                status.is_success() || status.is_redirection(),
                "{path}: {status}: {text}"
            );
            assert_eq!(stored_fields(app, id).await["field_note"], json!("b"));

            let fired: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM b71_form_submits WHERE item_id = $1")
                    .bind(id.to_string())
                    .fetch_one(&app.db)
                    .await
                    .unwrap();
            assert_eq!(fired, 1, "{path}: tap_form_submit fired {fired} times");
        }
    });
}

/// **Item 8.** `/item/{id}/edit` for a type with a Blocks field carries the
/// editor's scripts, pinned with SRI, and its glue script.
#[test]
fn an_item_form_with_a_blocks_field_carries_the_editor() {
    run(|app| async move {
        let (uid, cookies) = admin(app, "b71blocks").await;
        let id = seed_item(app, BLOCKS, uid, json!({})).await;
        for path in [
            format!("/item/{id}/edit"),
            format!("/admin/content/{id}/edit"),
        ] {
            let html = get_page(app, &cookies, &path).await;
            let editor = html
                .find("@editorjs/editorjs@2.31.5")
                .unwrap_or_else(|| panic!("{path}: no editor script"));
            let tag_start = html[..editor].rfind("<script").unwrap();
            let tag = &html[tag_start..editor + html[editor..].find('>').unwrap()];
            assert!(
                tag.contains(r#"integrity="sha384-"#),
                "{path}: no SRI: {tag}"
            );
            assert!(html.contains("/static/js/block-editor.js"), "{path}");
            assert!(
                !form_markup(&html).contains("<style"),
                "{path}: inline style in the form"
            );
        }
    });
}

/// **Item 3, rendering.** A cardinality -1 field renders every stored value
/// and one empty slot.
#[test]
fn an_unlimited_field_renders_every_value_and_one_empty_slot() {
    run(|app| async move {
        let (uid, cookies) = admin(app, "b71card").await;
        let id = seed_item(
            app,
            PROBE,
            uid,
            json!({"field_tags": ["one", "two", "three"]}),
        )
        .await;
        for path in [
            format!("/item/{id}/edit"),
            format!("/admin/content/{id}/edit"),
        ] {
            let html = get_page(app, &cookies, &path).await;
            for (i, value) in ["one", "two", "three"].iter().enumerate() {
                assert_eq!(
                    input_value(&html, &format!("field_tags[{i}]")).as_deref(),
                    Some(*value),
                    "{path}: stored value {i}"
                );
            }
            assert!(
                html.contains(r#"name="field_tags[3]""#),
                "{path}: no empty slot"
            );
            assert_eq!(
                input_value(&html, "field_tags[3]").unwrap_or_default(),
                "",
                "{path}: the empty slot is not empty"
            );
            assert!(!html.contains(r#"name="field_tags[4]""#), "{path}");
        }
    });
}

/// The non-admin item form sits in the site theme, not a bare document, and
/// carries no inline style.
#[test]
fn the_item_form_renders_inside_the_site_theme() {
    run(|app| async move {
        let (uid, cookies) = admin(app, "b71theme").await;
        let id = seed_item(app, PROBE, uid, json!({})).await;
        for path in [format!("/item/{id}/edit"), format!("/item/add/{PROBE}")] {
            let html = get_page(app, &cookies, &path).await;
            assert!(
                html.contains(r#"<div class="site-header">"#),
                "{path}: not in the site theme"
            );
            assert!(html.contains(r#"<nav class="site-nav""#), "{path}");
            assert!(html.contains("/static/css/forms.css"), "{path}");
            // The form brings no style of its own, and neither does the
            // theme: `base.html` links its palette from `static/css/base.css`.
            assert!(
                !form_markup(&html).contains("<style"),
                "{path}: inline style in the form"
            );
            assert_eq!(
                html.matches("<style").count(),
                0,
                "{path}: an inline style block"
            );
        }
    });
}

/// AI Assist buttons and their script arrive through `tap_form_alter`, for a
/// user who holds the AI permission and only for them.
#[test]
fn ai_assist_arrives_through_form_alter() {
    run(|app| async move {
        let (owner, _) = admin(app, "b71aiown").await;
        let id = seed_item(app, PROBE, owner, json!({"field_note": "x"})).await;
        let path = format!("/item/{id}/edit");

        let (_editor, cookies) = common::user_holding(
            app,
            "b71aied",
            &["access content", "edit any content", "use ai chat"],
        )
        .await;
        let html = get_page(app, &cookies, &path).await;
        assert!(
            html.contains(r#"class="ai-assist-btn""#),
            "no AI Assist button"
        );
        assert!(
            html.contains(r#"data-field="field_note""#),
            "no button on the note"
        );
        assert!(html.contains("/static/js/ai-assist.js"), "no AI script");

        let (_plain, cookies) =
            common::user_holding(app, "b71ainot", &["access content", "edit any content"]).await;
        let html = get_page(app, &cookies, &path).await;
        assert!(!html.contains("ai-assist-btn"), "button without permission");
        assert!(!html.contains("/static/js/ai-assist.js"));
    });
}

/// **B36.** The field list's "Add field" is an in-page anchor to the form on
/// the page; nothing on the page links to the POST-only route.
#[test]
fn the_field_list_does_not_link_to_a_post_only_route() {
    run(|app| async move {
        let (_uid, cookies) = admin(app, "b71b36").await;
        let html = get_page(
            app,
            &cookies,
            &format!("/admin/structure/types/{PROBE}/fields"),
        )
        .await;
        assert!(html.contains(r##"href="#add-field-form""##), "no anchor");
        assert!(html.contains(r#"id="add-field-form""#), "no anchor target");
        for (at, _) in html.match_indices("href=\"") {
            let href = &html[at + 6..at + 6 + html[at + 6..].find('"').unwrap()];
            let href = href.replace("&#x2F;", "/");
            assert!(
                !href.ends_with("/fields/add"),
                "links to the POST-only route: {href}"
            );
        }
    });
}

// ---------------------------------------------------------------------------
// Part 4: AJAX and conditional fields
// ---------------------------------------------------------------------------

/// The token and build id the page's AJAX requests use.
fn ajax_tokens(html: &str) -> (String, String) {
    (
        input_value(html, "_ajax_token").expect("the form carries an AJAX token"),
        input_value(html, "_form_build_id").expect("the form carries a build id"),
    )
}

/// POST `/system/ajax` as the page's script does. Returns the status, the
/// next token from the response header, and the body.
async fn post_ajax(
    app: &TestApp,
    cookies: &str,
    token: Option<&str>,
    build_id: &str,
    trigger: &str,
) -> (StatusCode, Option<String>, Value) {
    let mut request =
        Request::post("/system/ajax").header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        request = request.header("X-CSRF-Token", token);
    }
    let payload = json!({"form_build_id": build_id, "trigger": trigger, "values": {}});
    let response = app
        .request_with_cookies(
            request.body(Body::from(payload.to_string())).unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let next = response
        .headers()
        .get("X-CSRF-Token")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let body = body_string(response).await;
    (
        status,
        next,
        serde_json::from_str(&body).unwrap_or(Value::Null),
    )
}

/// A signed-in editor who is not an administrator.
async fn editor(app: &TestApp, prefix: &str) -> (Uuid, String) {
    common::user_holding(app, prefix, &["access content", "edit any content"]).await
}

/// **Item 4.** A non-admin editor on `/item/{id}/edit` gets "Add another", and
/// it works: the server builds the next value's input at the next delta, and
/// each response hands over the next token.
#[test]
fn a_non_admin_editor_adds_another_value() {
    run(|app| async move {
        let (owner, _) = admin(app, "b71addown").await;
        let id = seed_item(app, PROBE, owner, json!({"field_tags": ["one"]})).await;
        let (_editor, cookies) = editor(app, "b71adder").await;

        let html = get_page(app, &cookies, &format!("/item/{id}/edit")).await;
        assert!(
            html.contains(r#"data-ajax-trigger="add_field_tags""#),
            "no Add another for a non-admin editor"
        );
        let (token, build_id) = ajax_tokens(&html);

        // One stored value and one empty slot are on the page: the next is 2.
        let (status, next, body) =
            post_ajax(app, &cookies, Some(&token), &build_id, "add_field_tags").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let command = &body["commands"][0];
        assert_eq!(command["command"], "append", "{body}");
        assert_eq!(command["selector"], "#field_tags-items");
        assert!(
            command["html"]
                .as_str()
                .unwrap()
                .contains(r#"name="field_tags[2]""#),
            "{body}"
        );
        let next = next.expect("the response carries the next token");
        assert_ne!(next, token);

        // The spent token is refused; the next one works and moves on a delta.
        let (status, _, _) =
            post_ajax(app, &cookies, Some(&token), &build_id, "add_field_tags").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "a token is single use");
        let (status, _, body) =
            post_ajax(app, &cookies, Some(&next), &build_id, "add_field_tags").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(
            body["commands"][0]["html"]
                .as_str()
                .unwrap()
                .contains(r#"name="field_tags[3]""#),
            "{body}"
        );
    });
}

/// Every AJAX POST verifies CSRF: no header, or a token the session never
/// issued, is 403 and nothing runs.
#[test]
fn an_ajax_call_without_a_valid_csrf_header_is_refused() {
    run(|app| async move {
        let (uid, cookies) = admin(app, "b71ajaxcsrf").await;
        let id = seed_item(app, PROBE, uid, json!({})).await;
        let html = get_page(app, &cookies, &format!("/item/{id}/edit")).await;
        let (_token, build_id) = ajax_tokens(&html);

        for token in [None, Some("not-a-token")] {
            let (status, next, _) =
                post_ajax(app, &cookies, token, &build_id, "add_field_tags").await;
            assert_eq!(status, StatusCode::FORBIDDEN, "token {token:?}");
            assert!(next.is_none(), "a refused call gets no next token");
        }
    });
}

/// A user who may not open the form cannot drive its AJAX, even with a valid
/// token of their own and the form's build id.
#[test]
fn an_ajax_call_for_a_form_the_user_may_not_open_is_refused() {
    run(|app| async move {
        let (owner, owner_cookies) = admin(app, "b71ajaxown").await;
        let id = seed_item(app, PROBE, owner, json!({})).await;
        let html = get_page(app, &owner_cookies, &format!("/item/{id}/edit")).await;
        let (_token, build_id) = ajax_tokens(&html);

        let (_reader, cookies) = common::user_holding(app, "b71ajaxrd", &["access content"]).await;
        let own = get_page(app, &cookies, "/user/profile").await;
        let token = input_value(&own, "_token").expect("a token of the reader's own");
        let (status, _, body) =
            post_ajax(app, &cookies, Some(&token), &build_id, "add_field_tags").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    });
}

/// `tap_form_ajax` runs with the services: the fixture answers from its table.
#[test]
fn tap_form_ajax_can_read_the_database() {
    run(|app| async move {
        let (uid, cookies) = admin(app, "b71ajaxdb").await;
        let id = seed_item(app, PROBE, uid, json!({})).await;
        let html = get_page(app, &cookies, &format!("/item/{id}/edit")).await;
        let (token, build_id) = ajax_tokens(&html);

        let (status, _, body) =
            post_ajax(app, &cookies, Some(&token), &build_id, "b71_count").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let html = body["commands"][0]["html"].as_str().unwrap_or_default();
        assert!(
            html.contains("submissions"),
            "the handler had no database: {body}"
        );
    });
}

/// `visible_when`: hidden while false, its posted value dropped and its
/// `required` skipped; shown, saved and required while true.
#[test]
fn a_conditional_field_is_hidden_dropped_and_not_required_while_false() {
    run(|app| async move {
        let (uid, cookies) = admin(app, "b71when").await;
        for path in [
            "/item/{id}/edit".to_string(),
            "/admin/content/{id}/edit".to_string(),
        ] {
            let id = seed_item(
                app,
                WHEN,
                uid,
                json!({"field_online": false, "field_url": "kept"}),
            )
            .await;
            let path = path.replace("{id}", &id.to_string());

            let html = get_page(app, &cookies, &path).await;
            let at = html
                .find(r#"data-visible-when-field="field_online""#)
                .unwrap_or_else(|| panic!("{path}: no condition on the page"));
            let open = html[..at].rfind('<').unwrap();
            let tag = &html[open..at + html[at..].find('>').unwrap()];
            assert!(tag.contains("hidden"), "{path}: shown while false: {tag}");
            assert!(html.contains("/static/js/form-states.js"), "{path}");

            // Unchecked: whatever was typed is not saved, and the required
            // field does not block the save.
            let body = format!(
                "{}&title=When&field_url=typed+while+hidden",
                form_tokens(&html)
            );
            let (status, text) = post_form(app, &cookies, &path, body).await;
            assert!(
                status.is_success() || status.is_redirection(),
                "{path}: {status}: {text}"
            );
            assert_eq!(
                stored_fields(app, id).await["field_url"],
                json!("kept"),
                "{path}"
            );

            // Checked and empty: required.
            let html = get_page(app, &cookies, &path).await;
            let body = format!(
                "{}&title=When&field_online=1&field_url=",
                form_tokens(&html)
            );
            let (status, text) = post_form(app, &cookies, &path, body).await;
            assert!(!status.is_redirection(), "{path}: saved without the URL");
            assert!(text.contains("Stream URL is required."), "{path}");

            // Checked and filled: saved.
            let html = get_page(app, &cookies, &path).await;
            let body = format!(
                "{}&title=When&field_online=1&field_url=https%3A%2F%2Fexample.org",
                form_tokens(&html)
            );
            let (status, text) = post_form(app, &cookies, &path, body).await;
            assert!(
                status.is_success() || status.is_redirection(),
                "{path}: {status}: {text}"
            );
            let fields = stored_fields(app, id).await;
            assert_eq!(fields["field_url"], json!("https://example.org"), "{path}");
            assert_eq!(fields["field_online"], json!(true), "{path}");
        }
    });
}

/// The record reference autocomplete works from the non-admin form: the page
/// loads its script, and the endpoint answers the editor with the targets
/// they may view and not one on a stage they cannot read.
#[test]
fn record_autocomplete_works_from_the_item_form_and_only_offers_viewable_targets() {
    run(|app| async move {
        let (owner, _) = admin(app, "b71refown").await;
        let marker = format!("Zq{}", &Uuid::now_v7().simple().to_string()[24..]);
        let visible = seed_titled(
            app,
            REF_TARGET,
            owner,
            &format!("{marker} visible"),
            json!({}),
            None,
        )
        .await;
        let stage = common::create_test_stage(app, "b71ref").await;
        let staged = seed_titled(
            app,
            REF_TARGET,
            owner,
            &format!("{marker} staged"),
            json!({}),
            Some(stage),
        )
        .await;
        let id = seed_item(app, REF_PROBE, owner, json!({})).await;

        let (_editor, cookies) = editor(app, "b71refed").await;
        let html = get_page(app, &cookies, &format!("/item/{id}/edit")).await;
        assert!(
            html.contains("/static/js/record-ref.js"),
            "no autocomplete script"
        );
        assert!(
            html.contains(&format!(r#"data-target-type="{REF_TARGET}""#)),
            "no autocomplete input"
        );

        let response = app
            .request_with_cookies(
                Request::get(format!(
                    "/api/v1/items/autocomplete?type={REF_TARGET}&q={marker}"
                ))
                .body(Body::empty())
                .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let found: Value = serde_json::from_str(&body_string(response).await).unwrap();
        let ids: Vec<&str> = found
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["id"].as_str())
            .collect();
        assert!(ids.contains(&visible.to_string().as_str()), "{found}");
        assert!(
            !ids.contains(&staged.to_string().as_str()),
            "offered a target on a stage the editor cannot read: {found}"
        );
    });
}
