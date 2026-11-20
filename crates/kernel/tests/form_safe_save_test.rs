#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A form save writes what the form rendered and nothing else (B71 Part 2).
//!
//! Both item form routes built the stored field map from the POST body alone:
//! `extract_content_fields` on the admin form and `ItemSubmission::from_form`
//! on `/item/{id}/edit`. `Item::update` then replaced the whole map, so every
//! key the form did not render was erased by one save: a plugin written key, a
//! key the type does not declare, the values of a multi value field. A checked
//! Boolean was stored as the string `"1"`, and `cardinality` was never counted.
//!
//! These drive the real router: render the edit page, post back what a browser
//! would, and read the row.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app};
use serde_json::{Value, json};
use uuid::Uuid;

const TYPE: &str = "b71_safe_save";

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// Register the probe type: a text field, a Boolean, an unlimited multi value
/// field and one capped at two values.
async fn ensure_type(app: &TestApp) {
    use trovato_sdk::types::{FieldDefinition, FieldType};

    let fields = vec![
        FieldDefinition::new("field_headline", FieldType::Text { max_length: None })
            .label("Headline"),
        FieldDefinition::new("field_flag", FieldType::Boolean).label("Flag"),
        FieldDefinition::new("field_tags", FieldType::Text { max_length: None })
            .label("Tags")
            .cardinality(-1),
        FieldDefinition::new("field_pair", FieldType::Text { max_length: None })
            .label("Pair")
            .cardinality(2),
    ];
    let settings = json!({ "fields": serde_json::to_value(&fields).unwrap() });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'B71 Safe Save', '', true, 'Title', 'b71_test', $2) \
         ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
    )
    .bind(TYPE)
    .bind(&settings)
    .execute(&app.db)
    .await
    .unwrap();
    app.state
        .content_types()
        .replace(TYPE, "B71 Safe Save", None, settings)
        .await
        .ok();
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

/// The form's CSRF token, whichever input carries it, as `name=value`.
fn csrf_pair(html: &str) -> String {
    for name in ["_token", "_csrf"] {
        if let Some(token) = input_value(html, name) {
            return format!("{name}={}", urlencoding::encode(&token));
        }
    }
    panic!("the form carries no CSRF token");
}

/// The form's build id as `&_form_build_id=…`, or nothing if it prints none.
fn build_id_pair(html: &str) -> String {
    input_value(html, "_form_build_id")
        .map(|id| format!("&_form_build_id={}", urlencoding::encode(&id)))
        .unwrap_or_default()
}

/// Create the probe item through the service, as a plugin or import would.
async fn seed_item(app: &TestApp, author: Uuid, fields: Value) -> Uuid {
    let user = trovato_kernel::tap::UserContext::authenticated(
        author,
        vec!["administer site".to_string(), "publish content".to_string()],
    );
    let item = app
        .state
        .items()
        .create(
            trovato_kernel::models::CreateItem {
                item_type: TYPE.to_string(),
                title: "Safe save probe".to_string(),
                author_id: author,
                status: Some(1),
                promote: None,
                sticky: None,
                fields: Some(fields),
                stage_id: None,
                language: None,
                log: None,
            },
            &user,
        )
        .await
        .expect("seed item");
    item.id
}

async fn stored_fields(app: &TestApp, id: Uuid) -> Value {
    sqlx::query_scalar("SELECT fields FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

async fn get_page(app: &TestApp, cookies: &str, uri: &str) -> String {
    let page = app
        .request_with_cookies(
            Request::builder().uri(uri).body(Body::empty()).unwrap(),
            cookies,
        )
        .await;
    assert_eq!(page.status(), StatusCode::OK, "GET {uri}");
    body_string(page).await
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

/// The two edit routes, by the path their form posts to.
fn edit_paths(id: Uuid) -> [String; 2] {
    [
        format!("/item/{id}/edit"),
        format!("/admin/content/{id}/edit"),
    ]
}

async fn admin(app: &TestApp, name: &str) -> (Uuid, String) {
    let cookies = app
        .create_and_login_admin(
            name,
            "correct-horse-battery-staple",
            &format!("{name}@test.local"),
        )
        .await;
    (common::user_id_of(app, name).await, cookies)
}

/// **The finding.** One save from either edit form keeps a key the type does
/// not declare, a key a plugin wrote, and every value of a multi value field.
#[test]
fn an_edit_save_keeps_every_key_the_form_did_not_render() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let (uid, cookies) = admin(app, &common::username("b71keep")).await;

        for path in edit_paths(Uuid::nil()) {
            let id = seed_item(
                app,
                uid,
                json!({
                    "field_headline": "Before",
                    "field_tags": ["one", "two"],
                    "legacy_note": "a key the type does not declare",
                    "plugin_meta": {"written_by": "a plugin", "score": 7},
                }),
            )
            .await;
            let path = path.replace(&Uuid::nil().to_string(), &id.to_string());

            let html = get_page(app, &cookies, &path).await;
            let body = format!(
                "{}{}&title=Safe+save+probe&field_headline=After\
                 &field_tags%5B0%5D=one&field_tags%5B1%5D=two",
                csrf_pair(&html),
                build_id_pair(&html)
            );
            let (status, text) = post_form(app, &cookies, &path, body).await;
            assert!(
                status.is_success() || status.is_redirection(),
                "{path}: save refused ({status}): {text}"
            );

            let fields = stored_fields(app, id).await;
            assert_eq!(fields["field_headline"], json!("After"), "{path}: {fields}");
            assert_eq!(
                fields["legacy_note"],
                json!("a key the type does not declare"),
                "{path}: an undeclared key was erased: {fields}"
            );
            assert_eq!(
                fields["plugin_meta"],
                json!({"written_by": "a plugin", "score": 7}),
                "{path}: a plugin written key was erased: {fields}"
            );
            assert_eq!(
                fields["field_tags"],
                json!(["one", "two"]),
                "{path}: the multi value field lost a value: {fields}"
            );
        }
    });
}

/// A checked Boolean is JSON `true`; an unchecked one, which a browser does not
/// post at all, is JSON `false`, not left as it was.
#[test]
fn a_boolean_is_stored_as_a_json_boolean() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let (uid, cookies) = admin(app, &common::username("b71bool")).await;

        for path in edit_paths(Uuid::nil()) {
            let id = seed_item(app, uid, json!({"field_headline": "x"})).await;
            let path = path.replace(&Uuid::nil().to_string(), &id.to_string());

            let html = get_page(app, &cookies, &path).await;
            let body = format!(
                "{}{}&title=Bool&field_headline=x&field_flag=1",
                csrf_pair(&html),
                build_id_pair(&html)
            );
            let (status, text) = post_form(app, &cookies, &path, body).await;
            assert!(
                status.is_success() || status.is_redirection(),
                "{path}: {text}"
            );
            assert_eq!(
                stored_fields(app, id).await["field_flag"],
                json!(true),
                "{path}: a checked box must store JSON true"
            );

            let html = get_page(app, &cookies, &path).await;
            let body = format!(
                "{}{}&title=Bool&field_headline=x",
                csrf_pair(&html),
                build_id_pair(&html)
            );
            let (status, text) = post_form(app, &cookies, &path, body).await;
            assert!(
                status.is_success() || status.is_redirection(),
                "{path}: {text}"
            );
            assert_eq!(
                stored_fields(app, id).await["field_flag"],
                json!(false),
                "{path}: an unchecked box must store JSON false"
            );
        }
    });
}

/// More values than a field's cardinality is refused with an error that names
/// the field, and nothing is written.
#[test]
fn a_multi_value_field_over_its_cardinality_is_refused() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let (uid, cookies) = admin(app, &common::username("b71card")).await;

        for path in edit_paths(Uuid::nil()) {
            let id = seed_item(app, uid, json!({"field_pair": ["a", "b"]})).await;
            let path = path.replace(&Uuid::nil().to_string(), &id.to_string());

            let html = get_page(app, &cookies, &path).await;
            let body = format!(
                "{}{}&title=Card&field_pair%5B0%5D=a&field_pair%5B1%5D=b&field_pair%5B2%5D=c",
                csrf_pair(&html),
                build_id_pair(&html)
            );
            let (status, text) = post_form(app, &cookies, &path, body).await;
            assert!(
                text.contains("Pair allows at most 2 values"),
                "{path}: expected a field error, got {status}: {text}"
            );
            assert_eq!(
                stored_fields(app, id).await["field_pair"],
                json!(["a", "b"]),
                "{path}: a refused save must write nothing"
            );
        }
    });
}

/// The data migration rewrites the strings the forms used to store, in items
/// and revisions, for declared Boolean fields only.
#[test]
fn the_boolean_migration_rewrites_stored_strings() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let (uid, _cookies) = admin(app, &common::username("b71mig")).await;

        let on = seed_item(app, uid, json!({"field_flag": "1", "note": "1"})).await;
        let off = seed_item(app, uid, json!({"field_flag": "0"})).await;
        let typed = seed_item(app, uid, json!({"field_flag": true})).await;

        sqlx::raw_sql(sqlx::AssertSqlSafe(include_str!(
            "../migrations/20261120000001_boolean_fields_as_json.sql"
        )))
        .execute(&app.db)
        .await
        .expect("the migration runs, and runs again");

        let on_fields = stored_fields(app, on).await;
        assert_eq!(on_fields["field_flag"], json!(true));
        assert_eq!(
            on_fields["note"],
            json!("1"),
            "an undeclared key is not touched"
        );
        assert_eq!(stored_fields(app, off).await["field_flag"], json!(false));
        assert_eq!(stored_fields(app, typed).await["field_flag"], json!(true));

        let revision: Value = sqlx::query_scalar(
            "SELECT fields FROM item_revision WHERE item_id = $1 ORDER BY created DESC LIMIT 1",
        )
        .bind(on)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(revision["field_flag"], json!(true), "revisions too");
    });
}
