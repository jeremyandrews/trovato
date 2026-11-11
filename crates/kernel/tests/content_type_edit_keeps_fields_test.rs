#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Saving a content type's edit form, or submitting the add form with a
//! machine name that already exists, never removes a field or any other
//! settings key from that type.
//!
//! `ContentTypeRegistry::update` wrote the edit form's three settings keys over
//! the whole `item_type.settings` column, so the `fields` key, and any other key
//! the type carried, was gone from the row. The cache entry it wrote kept the
//! previous field list, so the admin went on showing the fields until the next
//! reload from the database, and then every field disappeared from the type.
//!
//! `add_content_type_submit` asked the cache whether a machine name was taken,
//! and `ContentTypeRegistry::create` upserted, so a row the database held but
//! this process had not cached yet was overwritten, fields and all.
//!
//! These drive the real routes over HTTP, the way
//! `content_type_duplicate_field_test.rs` does.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::{CreateItemType, ItemType};

// One scratch type per test: the tests in this binary run in parallel against
// one shared database.
const EDIT_TYPE: &str = "keep_fields_edit";
const RELOAD_TYPE: &str = "keep_fields_reload";
const ADD_TYPE: &str = "keep_fields_add";
const FIELD_EDIT_TYPE: &str = "keep_fields_field_edit";

const PASSWORD: &str = "correct-horse-battery-staple";

async fn body_string(response: Response) -> String {
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

/// Encode a form value. The values here are plain ASCII words and spaces.
fn enc(value: &str) -> String {
    value.replace(' ', "+")
}

fn field(name: &str, label: &str) -> serde_json::Value {
    serde_json::json!({
        "field_name": name,
        "field_type": "TextLong",
        "label": label,
        "required": false,
        "cardinality": 1,
        "settings": {},
        "personal_data": false,
    })
}

/// The settings every scratch type starts with: two fields, the form's keys,
/// and a key no code knows, which a writer that replaces the column drops.
fn seed_settings() -> serde_json::Value {
    serde_json::json!({
        "fields": [field("field_alpha", "Alpha"), field("field_beta", "Beta")],
        "title_label": "Headline",
        "published_default": false,
        "revision_default": false,
        "probe_key": "kept",
    })
}

/// Create the scratch type with two fields and `probe_key`, and register it
/// for removal when the test ends.
async fn seed_scratch_type(app: &TestApp, type_name: &str) {
    app.cleanup_content_type_on_exit(type_name);
    sqlx::query("DELETE FROM item_type WHERE type = $1")
        .bind(type_name)
        .execute(&app.db)
        .await
        .expect("clear any row an earlier run left");
    app.state.content_types().invalidate(type_name);

    app.state
        .content_types()
        .create(
            type_name,
            "Keep Fields Type",
            Some("Scratch type for the edit-keeps-fields tests."),
            seed_settings(),
        )
        .await
        .expect("create the scratch content type");
}

async fn stored_settings(app: &TestApp, type_name: &str) -> serde_json::Value {
    sqlx::query_scalar("SELECT settings FROM item_type WHERE type = $1")
        .bind(type_name)
        .fetch_one(&app.db)
        .await
        .expect("the scratch type must have a row")
}

fn field_names(settings: &serde_json::Value) -> Vec<String> {
    settings
        .get("fields")
        .and_then(|v| v.as_array())
        .map(|fields| {
            fields
                .iter()
                .map(|f| {
                    f.get("field_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("<unnamed>")
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn both_fields() -> Vec<String> {
    vec!["field_alpha".to_string(), "field_beta".to_string()]
}

/// GET a form page and return (cookies, csrf token, build id).
async fn open_form(app: &TestApp, cookies: &str, path: &str) -> (String, String, String) {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    assert_eq!(response.status(), StatusCode::OK, "{path} must render");

    let fresh = common::extract_cookies(&response);
    let cookies = if fresh.is_empty() {
        cookies.to_string()
    } else {
        fresh
    };
    let html = body_string(response).await;
    let csrf = input_value(&html, "_token").expect("the form must carry a CSRF token");
    let build_id = input_value(&html, "_form_build_id").expect("the form must carry a build id");
    (cookies, csrf, build_id)
}

async fn post_form(app: &TestApp, cookies: &str, path: &str, body: String) -> Response {
    app.request_with_cookies(
        Request::post(path)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
}

/// Post the type's edit form with a new label and title label, as an admin
/// would after changing those two inputs.
async fn post_edit_form(app: &TestApp, cookies: &str, type_name: &str) {
    let path = format!("/admin/structure/types/{type_name}/edit");
    let (cookies, csrf, build_id) = open_form(app, cookies, &path).await;
    let body = format!(
        "_token={csrf}&_form_build_id={build_id}&label={}&machine_name={type_name}\
         &description={}&title_label={}&published_default=1",
        enc("Renamed Type"),
        enc("Edited description"),
        enc("Display name"),
    );
    let response = post_form(app, &cookies, &path, body).await;
    assert!(
        response.status().is_redirection(),
        "the edit form must be accepted, got {}",
        response.status()
    );
}

/// **The finding, in the row.** Saving the edit form keeps the type's fields
/// and every settings key the form does not own.
#[test]
fn the_edit_form_keeps_the_types_fields_and_other_settings() {
    run_test(async {
        let app = shared_app().await;
        seed_scratch_type(app, EDIT_TYPE).await;
        let cookies = app
            .create_and_login_admin("keepfieldsedit", PASSWORD, "keepfieldsedit@test.local")
            .await;

        post_edit_form(app, &cookies, EDIT_TYPE).await;

        let settings = stored_settings(app, EDIT_TYPE).await;
        assert_eq!(
            field_names(&settings),
            both_fields(),
            "saving the edit form must not remove the type's fields; settings are now {settings}"
        );
        assert_eq!(
            settings.get("probe_key").and_then(|v| v.as_str()),
            Some("kept"),
            "saving the edit form must not remove a settings key it does not own; settings are \
             now {settings}"
        );
        assert_eq!(
            settings.get("title_label").and_then(|v| v.as_str()),
            Some("Display name"),
            "the edit must still have written the new title label into settings"
        );
        assert_eq!(
            settings.get("published_default").and_then(|v| v.as_bool()),
            Some(true),
            "the edit must still have written published_default"
        );

        let (label, title_label): (String, Option<String>) =
            sqlx::query_as("SELECT label, title_label FROM item_type WHERE type = $1")
                .bind(EDIT_TYPE)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(label, "Renamed Type", "the label must have changed");
        assert_eq!(
            title_label.as_deref(),
            Some("Display name"),
            "the title label column must have changed"
        );
    });
}

/// **The finding as an admin sees it.** The cache hid the loss until the next
/// reload from the database; after one, the type must still declare its
/// fields.
#[test]
fn the_edit_form_keeps_the_types_fields_across_a_reload() {
    run_test(async {
        let app = shared_app().await;
        seed_scratch_type(app, RELOAD_TYPE).await;
        let cookies = app
            .create_and_login_admin("keepfieldsreload", PASSWORD, "keepfieldsreload@test.local")
            .await;

        post_edit_form(app, &cookies, RELOAD_TYPE).await;

        app.state
            .content_types()
            .reload_from_db()
            .await
            .expect("reload the registry from the database");

        let def = app
            .state
            .content_types()
            .get(RELOAD_TYPE)
            .expect("the type must still be registered after a reload");
        let names: Vec<String> = def.fields.iter().map(|f| f.field_name.clone()).collect();
        assert_eq!(
            names,
            both_fields(),
            "after the edit form and a reload from the database the type must still declare \
             both fields"
        );
        assert_eq!(def.label, "Renamed Type");
    });
}

/// **The add path.** A row the database holds but this process has not cached
/// (written by a second server, or a config import before the next reload) is
/// refused by machine name, and left exactly as it was.
#[test]
fn the_add_form_refuses_a_machine_name_only_the_database_knows() {
    run_test(async {
        let app = shared_app().await;
        app.cleanup_content_type_on_exit(ADD_TYPE);
        sqlx::query("DELETE FROM item_type WHERE type = $1")
            .bind(ADD_TYPE)
            .execute(&app.db)
            .await
            .expect("clear any row an earlier run left");

        // Straight into the table, bypassing the registry and its cache.
        ItemType::create(
            &app.db,
            CreateItemType {
                type_name: ADD_TYPE.to_string(),
                label: "Database Only".to_string(),
                description: Some("Written where this process cannot see it.".to_string()),
                has_title: Some(true),
                title_label: Some("Headline".to_string()),
                plugin: "core".to_string(),
                settings: Some(seed_settings()),
            },
        )
        .await
        .expect("insert the row directly");
        app.state.content_types().invalidate(ADD_TYPE);
        assert!(
            app.state.content_types().get(ADD_TYPE).is_none(),
            "the precondition is a row the cache does not know"
        );

        let snapshot = || async {
            let row: String =
                sqlx::query_scalar("SELECT row_to_json(t)::text FROM item_type t WHERE type = $1")
                    .bind(ADD_TYPE)
                    .fetch_one(&app.db)
                    .await
                    .expect("the row must still exist");
            row
        };
        let before = snapshot().await;

        let cookies = app
            .create_and_login_admin("keepfieldsadd", PASSWORD, "keepfieldsadd@test.local")
            .await;
        let (cookies, csrf, build_id) =
            open_form(app, &cookies, "/admin/structure/types/add").await;
        let body = format!(
            "_token={csrf}&_form_build_id={build_id}&label={}&machine_name={ADD_TYPE}\
             &description={}&title_label={}",
            enc("Overwritten"),
            enc("From the add form"),
            enc("Name"),
        );
        let response = post_form(app, &cookies, "/admin/structure/types/add", body).await;
        let status = response.status();
        let html = body_string(response).await.replace("&#x27;", "'");

        let after = snapshot().await;
        assert_eq!(
            after, before,
            "submitting the add form with a taken machine name must leave the row byte for byte \
             unchanged"
        );
        assert_eq!(
            status,
            StatusCode::OK,
            "the add form must come back with an error, not redirect as if it had created \
             something"
        );
        assert!(
            html.contains(&format!(
                "A content type with machine name '{ADD_TYPE}' already exists."
            )),
            "the add form must say the machine name is taken"
        );
    });
}

/// **The two writers against each other.** A type edit and then a field edit:
/// the field writer must not lose what the type writer stored, nor the type
/// writer the fields.
#[test]
fn a_field_edit_after_a_type_edit_keeps_both_fields() {
    run_test(async {
        let app = shared_app().await;
        seed_scratch_type(app, FIELD_EDIT_TYPE).await;
        let cookies = app
            .create_and_login_admin(
                "keepfieldsfieldedit",
                PASSWORD,
                "keepfieldsfieldedit@test.local",
            )
            .await;

        post_edit_form(app, &cookies, FIELD_EDIT_TYPE).await;

        let path = format!("/admin/structure/types/{FIELD_EDIT_TYPE}/fields/field_beta/edit");
        let (cookies, csrf, build_id) = open_form(app, &cookies, &path).await;
        let body = format!(
            "_token={csrf}&_form_build_id={build_id}&label={}&cardinality=1",
            enc("Beta renamed"),
        );
        let response = post_form(app, &cookies, &path, body).await;
        assert!(
            response.status().is_redirection(),
            "the field edit must be accepted, got {}",
            response.status()
        );

        let settings = stored_settings(app, FIELD_EDIT_TYPE).await;
        assert_eq!(
            field_names(&settings),
            both_fields(),
            "a field edit after a type edit must leave both fields; settings are now {settings}"
        );
        let beta_label = settings["fields"][1]
            .get("label")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        assert_eq!(beta_label.as_deref(), Some("Beta renamed"));
        assert_eq!(
            settings.get("title_label").and_then(|v| v.as_str()),
            Some("Display name"),
            "the field edit must not undo the type edit's title label"
        );

        app.state.content_types().reload_from_db().await.unwrap();
        let def = app.state.content_types().get(FIELD_EDIT_TYPE).unwrap();
        let names: Vec<String> = def.fields.iter().map(|f| f.field_name.clone()).collect();
        assert_eq!(names, both_fields(), "and so after a reload");
        assert_eq!(def.label, "Renamed Type");
    });
}
