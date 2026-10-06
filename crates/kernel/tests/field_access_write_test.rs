#![allow(clippy::unwrap_used, clippy::expect_used)]
//! S4 — item **writes** honour field-level edit access, over real HTTP.
//!
//! Field access was built and tested for reading: a plugin can hide a field
//! from a viewer and every read path honours it. Nothing honoured it on the way
//! in. Any user who could edit an item could overwrite a field they were not
//! even allowed to see, and the edit form showed them its current value.
//!
//! These drive the real router with the real reference plugin
//! (`plugins/trovato_field_access_ref`) enabled, whose default rules make
//! `person.ssn` require `view pii`. A user who may create and edit content but
//! does not hold `view pii` is refused the field on create and on update, is
//! not shown it on the edit form, and — the part that is easy to get wrong —
//! can still save an edit that leaves the field out, without erasing it.
//!
//! # Why its own file
//!
//! The read-side HTTP tests live in `field_access_rest_test.rs` on the shared
//! `TestApp`, which has no plugins enabled. Enabling one needs an `AppState`
//! built after the database says so, so this file builds its own app in the
//! shape `plugin_api_test.rs` and `recovery_plugin_flow_test.rs` use. Putting
//! that app in the read-side file would have left one binary building two
//! `AppState`s: two wasmtime pooling allocators (~64 GB of address space each,
//! which is why `common/mod.rs` shares one app at all), and two concurrent
//! `AppState::new` calls racing on plugin migrations — the race
//! `recovery_plugin_flow_test` documents.
//!
//! Requires Postgres + Redis and the fixture `.wasm` built into
//! `plugins/trovato_field_access_ref/`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, test_ip_for, user_holding};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const PLUGIN: &str = "trovato_field_access_ref";
const TYPE: &str = "person";

/// Advisory-lock key guarding the `person` fixture seeding, for the same reason
/// `common::CONFERENCE_SEED_LOCK` exists: every test binary shares one
/// database, so a check-then-insert has to be serialized in the database.
const PERSON_SEED_LOCK: i64 = 0x_C0FF_EE00_0054;

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("field-access fixture app init thread panicked")
    })
}

/// Install and enable the reference plugin, then build an app that loads it.
///
/// `AppState` resolves its enabled plugin set at construction, so the database
/// has to say "enabled" before the app exists.
async fn build_app() -> TestApp {
    trovato_test_utils::env::load_dotenv();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("failed to connect for fixture setup");
    trovato_kernel::plugin::status::install_plugin(&pool, PLUGIN, "1.0.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{PLUGIN}': {e:#}"));
    pool.close().await;

    TestApp::with_config(|config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
    })
    .await
}

/// Leave the fixture disabled so it does not load in other test binaries.
async fn disable_plugin(app: &TestApp) {
    sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = $1")
        .bind(PLUGIN)
        .execute(&app.db)
        .await
        .expect("disable the fixture plugin");
}

/// Seed the `person` type the plugin's default rules govern: `ssn` needs
/// `view pii`, `salary` needs `view salary`, `bio` is ungoverned.
async fn ensure_person_type(app: &TestApp) {
    use trovato_sdk::types::{FieldDefinition, FieldType};

    let fields = vec![
        FieldDefinition::new("ssn", FieldType::Text { max_length: None }).label("SSN"),
        FieldDefinition::new("salary", FieldType::Text { max_length: None }).label("Salary"),
        FieldDefinition::new("bio", FieldType::Text { max_length: None }).label("Bio"),
    ];
    let settings = serde_json::json!({
        "fields": serde_json::to_value(&fields).unwrap(),
        "title_label": "Name",
    });

    let mut tx = app.db.begin().await.expect("begin person type seed");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(PERSON_SEED_LOCK)
        .execute(&mut *tx)
        .await
        .expect("take person seed lock");
    sqlx::query(
        r#"INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings)
           VALUES ($1, 'Person', 'A person, for field-access tests', true, 'Name', 'core', $2)
           ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings"#,
    )
    .bind(TYPE)
    .bind(&settings)
    .execute(&mut *tx)
    .await
    .expect("seed the person item type");
    tx.commit().await.expect("commit person type seed");

    app.state
        .content_types()
        .create(
            TYPE,
            "Person",
            Some("A person, for field-access tests"),
            settings,
        )
        .await
        .ok();
}

/// The permissions a content editor who may not see PII holds.
const EDITOR: &[&str] = &[
    "create person content",
    "edit any content",
    "access content",
];

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// Pull an input's value out of rendered form HTML.
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

/// GET a page as this user, in their own rate-limit bucket.
async fn get_as(app: &TestApp, path: &str, cookies: &str, bucket: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(path)
                .header("x-forwarded-for", test_ip_for(bucket))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

/// The CSRF token the rendered form carries, which a JSON client sends in the
/// `X-CSRF-Token` header.
async fn csrf_from(app: &TestApp, path: &str, cookies: &str, bucket: &str) -> String {
    let (status, html) = get_as(app, path, cookies, bucket).await;
    assert_eq!(status, StatusCode::OK, "{path} should render for this user");
    input_value(&html, "_csrf").expect("the form must carry a CSRF token")
}

/// POST a JSON body as this user.
async fn post_json(
    app: &TestApp,
    path: &str,
    cookies: &str,
    bucket: &str,
    csrf: &str,
    body: serde_json::Value,
) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::post(path)
                .header(header::CONTENT_TYPE, "application/json")
                .header("X-CSRF-Token", csrf)
                .header("x-forwarded-for", test_ip_for(bucket))
                .body(Body::from(body.to_string()))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

/// An item with an `ssn`, created by an administrator so the gates bypass.
async fn seed_person(app: &TestApp, title: &str, ssn: &str) -> Uuid {
    let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
    app.state
        .items()
        .create(
            CreateItem {
                item_type: TYPE.to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({ "ssn": ssn, "bio": "before" })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("S4 write test".to_string()),
            },
            &admin,
        )
        .await
        .expect("seed person")
        .id
}

/// The `fields` object as the database holds it, read directly so no service
/// cache can answer for it.
async fn stored_fields(app: &TestApp, id: Uuid) -> serde_json::Value {
    sqlx::query_scalar("SELECT fields FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .expect("read stored fields")
}

#[test]
fn creating_an_item_with_a_denied_field_is_refused() {
    run_test(async {
        let app = app();
        ensure_person_type(app).await;
        let (_, cookies) = user_holding(app, "fieldwrite-create", EDITOR).await;
        let csrf = csrf_from(app, "/item/add/person", &cookies, "fw-create").await;

        // The finding, stated: a user who may create content submitting a field
        // they are not allowed to see.
        let (status, body) = post_json(
            app,
            "/item/add/person",
            &cookies,
            "fw-create",
            &csrf,
            serde_json::json!({
                "title": "Refused create",
                "fields": { "bio": "hello", "ssn": "123-45-6789" }
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "submitting `ssn` without `view pii` must be 403, got {status}: {body}"
        );
        assert!(
            body.contains("ssn"),
            "the refusal should name the field, got {body}"
        );

        // And the same create without it succeeds, so the gate refuses the
        // field rather than the user. A fresh token: they are single-use, and
        // the refused POST above consumed that one.
        let csrf = csrf_from(app, "/item/add/person", &cookies, "fw-create").await;
        let (status, body) = post_json(
            app,
            "/item/add/person",
            &cookies,
            "fw-create",
            &csrf,
            serde_json::json!({
                "title": "Allowed create",
                "fields": { "bio": "hello" }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "create without `ssn`: {body}");

        disable_plugin(app).await;
    });
}

#[test]
fn editing_a_denied_field_is_refused_and_the_stored_value_survives() {
    run_test(async {
        let app = app();
        ensure_person_type(app).await;
        let id = seed_person(app, "Edited person", "123-45-6789").await;
        let (_, cookies) = user_holding(app, "fieldwrite-edit", EDITOR).await;
        let path = format!("/item/{id}/edit");
        let csrf = csrf_from(app, &path, &cookies, "fw-edit").await;

        // Changing it: refused, and the stored value is untouched.
        let (status, body) = post_json(
            app,
            &path,
            &cookies,
            "fw-edit",
            &csrf,
            serde_json::json!({
                "title": "Edited person",
                "fields": { "bio": "after", "ssn": "999-99-9999" }
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "changing `ssn` without `view pii` must be 403, got {status}: {body}"
        );
        let fields = stored_fields(app, id).await;
        assert_eq!(
            fields.get("ssn").and_then(|v| v.as_str()),
            Some("123-45-6789"),
            "the refused write must not have landed"
        );
        assert_eq!(
            fields.get("bio").and_then(|v| v.as_str()),
            Some("before"),
            "a refusal is all-or-nothing: the rest of the submission is not saved either"
        );

        // Leaving it out: allowed, and it is not erased — the copy-back. A JSON
        // update replaces the whole `fields` object, so without it an editor
        // saving the fields they can see would silently delete the one they
        // cannot. (A fresh token: they are single-use.)
        let csrf = csrf_from(app, &path, &cookies, "fw-edit").await;
        let (status, body) = post_json(
            app,
            &path,
            &cookies,
            "fw-edit",
            &csrf,
            serde_json::json!({
                "title": "Edited person",
                "fields": { "bio": "after" }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "edit without `ssn`: {body}");
        let fields = stored_fields(app, id).await;
        assert_eq!(
            fields.get("bio").and_then(|v| v.as_str()),
            Some("after"),
            "the editable field was saved"
        );
        assert_eq!(
            fields.get("ssn").and_then(|v| v.as_str()),
            Some("123-45-6789"),
            "the denied field must survive a submission that left it out"
        );

        disable_plugin(app).await;
    });
}

#[test]
fn the_edit_form_shows_neither_the_input_nor_the_value() {
    run_test(async {
        let app = app();
        ensure_person_type(app).await;
        let id = seed_person(app, "Unseen person", "555-55-5555").await;
        let (_, cookies) = user_holding(app, "fieldwrite-form", EDITOR).await;

        let (status, html) = get_as(app, &format!("/item/{id}/edit"), &cookies, "fw-form").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !html.contains(r#"name="ssn""#),
            "the form must not render an input for a field this user may not see"
        );
        assert!(
            !html.contains("555-55-5555"),
            "and must not render its value anywhere on the page"
        );
        assert!(
            html.contains(r#"name="bio""#),
            "the fields they may edit are still there"
        );

        disable_plugin(app).await;
    });
}

#[test]
fn the_permission_holder_writes_the_field_through_the_same_paths() {
    run_test(async {
        let app = app();
        ensure_person_type(app).await;
        let id = seed_person(app, "Permitted person", "123-45-6789").await;
        let mut perms = EDITOR.to_vec();
        perms.push("view pii");
        let (_, cookies) = user_holding(app, "fieldwrite-pii", &perms).await;

        // The form offers the field.
        let path = format!("/item/{id}/edit");
        let (status, html) = get_as(app, &path, &cookies, "fw-pii").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains(r#"name="ssn""#),
            "`view pii` should see the input"
        );
        let csrf = input_value(&html, "_csrf").expect("csrf token");

        // And the write lands.
        let (status, body) = post_json(
            app,
            &path,
            &cookies,
            "fw-pii",
            &csrf,
            serde_json::json!({
                "title": "Permitted person",
                "fields": { "bio": "after", "ssn": "999-99-9999" }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "edit with `view pii`: {body}");
        assert_eq!(
            stored_fields(app, id)
                .await
                .get("ssn")
                .and_then(|v| v.as_str()),
            Some("999-99-9999")
        );

        // Creating with the field works too.
        let csrf = csrf_from(app, "/item/add/person", &cookies, "fw-pii").await;
        let (status, body) = post_json(
            app,
            "/item/add/person",
            &cookies,
            "fw-pii",
            &csrf,
            serde_json::json!({
                "title": "Permitted create",
                "fields": { "ssn": "111-11-1111" }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "create with `view pii`: {body}");

        disable_plugin(app).await;
    });
}

#[test]
fn anonymous_gets_no_edit_form_and_no_write() {
    run_test(async {
        let app = app();
        ensure_person_type(app).await;
        let id = seed_person(app, "Anonymous target", "777-77-7777").await;

        let (status, _) = get_as(app, &format!("/item/{id}/edit"), "", "fw-anon").await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "anonymous must not get the edit form"
        );

        // No session, so no CSRF token either: the write is refused before the
        // gates are reached, which is the point — they are not the only check.
        let (status, _) = post_json(
            app,
            &format!("/item/{id}/edit"),
            "",
            "fw-anon",
            "not-a-token",
            serde_json::json!({ "fields": { "ssn": "000-00-0000" } }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "anonymous must not write");
        assert_eq!(
            stored_fields(app, id)
                .await
                .get("ssn")
                .and_then(|v| v.as_str()),
            Some("777-77-7777")
        );

        disable_plugin(app).await;
    });
}
