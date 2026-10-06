#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Adding a field whose machine name the type already has is refused.
//!
//! `ContentTypeRegistry::add_field` pushed onto `settings->fields` without
//! looking at what was already in the list, and neither admin route checked
//! either, so a type could end up holding two or three definitions of the same
//! field. Found 2026-10-05 from the test side: a test that added
//! `search_test_field` to `page` on every run reached three copies, and at that
//! point the content translation form rendered none of the type's fields at
//! all.
//!
//! The check lives in the registry, so every path that adds a field inherits
//! it; these drive the two that exist, the form post and the AJAX callback,
//! over HTTP.
//!
//! Two defects in the same function are covered here as well: `add_field`
//! replaced the whole `settings` object instead of merging into it, dropping
//! `title_label` and `published_default`, and an unknown `field_type` string
//! silently became `Text`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use common::{TestApp, run_test, shared_app};

// One scratch content type per test, removed when that test ends. They are
// separate names because the targets in this binary run in parallel and share
// one database: a single shared name had the tests deleting and recreating each
// other's row mid-request.
const FORM_TYPE: &str = "dup_field_form";
const AJAX_TYPE: &str = "dup_field_ajax";
const MERGE_TYPE: &str = "dup_field_merge";
const UNKNOWN_TYPE: &str = "dup_field_unknown";

async fn body_string(response: Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
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

/// Create the scratch type, with two non-field settings keys so that a writer
/// which replaces `settings` wholesale instead of merging into it is caught.
///
/// Registered for removal through `cleanup_content_type_on_exit`, because this
/// binary shares one never-migrated database with every other test target and
/// with its own earlier runs.
async fn seed_scratch_type(app: &TestApp, type_name: &str) {
    app.cleanup_content_type_on_exit(type_name);
    sqlx::query("DELETE FROM item_type WHERE type = $1")
        .bind(type_name)
        .execute(&app.db)
        .await
        .expect("clear any row an earlier run left");

    app.state
        .content_types()
        .create(
            type_name,
            "Duplicate Field Type",
            Some("Scratch type for the duplicate field name tests."),
            serde_json::json!({
                "fields": [],
                "title_label": "Headline",
                "published_default": false,
            }),
        )
        .await
        .expect("create the scratch content type");
}

/// Every field definition on the type, in stored order, by machine name.
async fn stored_field_names(app: &TestApp, type_name: &str) -> Vec<String> {
    let settings: serde_json::Value =
        sqlx::query_scalar("SELECT settings FROM item_type WHERE type = $1")
            .bind(type_name)
            .fetch_one(&app.db)
            .await
            .expect("the scratch type must have a row");

    settings
        .get("fields")
        .and_then(|v| v.as_array())
        .expect("settings must carry a 'fields' array")
        .iter()
        .map(|f| {
            f.get("field_name")
                .and_then(|v| v.as_str())
                .unwrap_or("<unnamed>")
                .to_string()
        })
        .collect()
}

/// Render the manage-fields page and return (cookies, csrf token, build id).
///
/// A fresh pair is needed for every post: the token is single use and the build
/// id keys the form state the AJAX callback loads.
async fn open_fields_page(
    app: &TestApp,
    cookies: &str,
    type_name: &str,
) -> (String, String, String) {
    let response = app
        .request_with_cookies(
            Request::get(format!("/admin/structure/types/{type_name}/fields"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the manage fields page must render"
    );

    let fresh = common::extract_cookies(&response);
    let cookies = if fresh.is_empty() {
        cookies.to_string()
    } else {
        fresh
    };
    let html = body_string(response).await;
    let csrf = input_value(&html, "_token").expect("the add field form must carry a CSRF token");
    let build_id =
        input_value(&html, "_form_build_id").expect("the add field form must carry a build id");
    (cookies, csrf, build_id)
}

/// Post the add-field form once. Returns the response.
async fn post_add_field(
    app: &TestApp,
    cookies: &str,
    type_name: &str,
    csrf: &str,
    build_id: &str,
    label: &str,
    name: &str,
    field_type: &str,
) -> Response {
    let body = format!(
        "_token={csrf}&_form_build_id={build_id}&label={label}&name={name}&field_type={field_type}"
    );
    app.request_with_cookies(
        Request::post(format!("/admin/structure/types/{type_name}/fields/add"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
}

/// Fire the `add_field` AJAX trigger the way the page's button does.
async fn post_ajax_add_field(
    app: &TestApp,
    cookies: &str,
    csrf: &str,
    build_id: &str,
    label: &str,
    name: &str,
    field_type: &str,
) -> Response {
    let payload = serde_json::json!({
        "form_build_id": build_id,
        "trigger": "add_field",
        "values": {
            "label": label,
            "name": name,
            "field_type": field_type,
        },
    });
    app.request_with_cookies(
        Request::post("/system/ajax")
            .header(header::CONTENT_TYPE, "application/json")
            .header("X-CSRF-Token", csrf)
            .body(Body::from(payload.to_string()))
            .unwrap(),
        cookies,
    )
    .await
}

/// **The finding, on the form post.** The second add with the same machine name
/// is refused, and the list still holds one definition rather than two.
#[test]
fn the_form_refuses_a_field_name_the_type_already_has() {
    run_test(async {
        let app = shared_app().await;
        seed_scratch_type(app, FORM_TYPE).await;

        let cookies = app
            .create_and_login_admin(
                "dupfieldform",
                "correct-horse-battery-staple",
                "dupfieldform@test.local",
            )
            .await;

        // First add: this is the one that must succeed.
        let (cookies, csrf, build_id) = open_fields_page(app, &cookies, FORM_TYPE).await;
        let first = post_add_field(
            app,
            &cookies,
            FORM_TYPE,
            &csrf,
            &build_id,
            "Subtitle",
            "field_subtitle",
            "text",
        )
        .await;
        assert!(
            first.status().is_redirection(),
            "the first add must be accepted, got {}",
            first.status()
        );
        assert_eq!(
            stored_field_names(app, FORM_TYPE).await,
            vec!["field_subtitle".to_string()],
            "the first add must have written exactly one field"
        );

        // Second add, same machine name, different label.
        let (cookies, csrf, build_id) = open_fields_page(app, &cookies, FORM_TYPE).await;
        let second = post_add_field(
            app,
            &cookies,
            FORM_TYPE,
            &csrf,
            &build_id,
            "Subtitle+again",
            "field_subtitle",
            "text_long",
        )
        .await;

        let status = second.status();
        let html = body_string(second).await;

        assert_eq!(
            stored_field_names(app, FORM_TYPE).await,
            vec!["field_subtitle".to_string()],
            "a second add with a name the type already has must not append a duplicate"
        );
        assert!(
            html.contains("A field with machine name field_subtitle already exists on this type."),
            "the form must say which machine name collided; got {status} and a page that did not \
             mention it"
        );
        assert!(
            html.contains(r#"value="Subtitle again""#),
            "the refused form must come back with the submitted label still in it"
        );
        assert!(
            html.contains(r#"value="field_subtitle""#),
            "the refused form must come back with the submitted machine name still in it"
        );
    });
}

/// **The same finding on the AJAX path**, which is the one the page's button
/// actually uses.
#[test]
fn the_ajax_path_refuses_a_field_name_the_type_already_has() {
    run_test(async {
        let app = shared_app().await;
        seed_scratch_type(app, AJAX_TYPE).await;

        let cookies = app
            .create_and_login_admin(
                "dupfieldajax",
                "correct-horse-battery-staple",
                "dupfieldajax@test.local",
            )
            .await;

        let (cookies, csrf, build_id) = open_fields_page(app, &cookies, AJAX_TYPE).await;
        let first = post_ajax_add_field(
            app,
            &cookies,
            &csrf,
            &build_id,
            "Teaser",
            "field_teaser",
            "text",
        )
        .await;
        assert_eq!(
            first.status(),
            StatusCode::OK,
            "the first AJAX add must be accepted"
        );
        let first_body = body_string(first).await;
        assert!(
            first_body.contains("field_teaser"),
            "the first AJAX add must append the new row; got {first_body}"
        );
        assert_eq!(
            stored_field_names(app, AJAX_TYPE).await,
            vec!["field_teaser".to_string()],
            "the first AJAX add must have written exactly one field"
        );

        let (cookies, csrf, build_id) = open_fields_page(app, &cookies, AJAX_TYPE).await;
        let second = post_ajax_add_field(
            app,
            &cookies,
            &csrf,
            &build_id,
            "Teaser+again",
            "field_teaser",
            "text",
        )
        .await;
        assert_eq!(second.status(), StatusCode::OK);
        let body = body_string(second).await;

        assert_eq!(
            stored_field_names(app, AJAX_TYPE).await,
            vec!["field_teaser".to_string()],
            "a second AJAX add with a name the type already has must not append a duplicate"
        );
        assert!(
            body.contains("A field with machine name field_teaser already exists on this type."),
            "the AJAX response must alert with the colliding machine name; got {body}"
        );
    });
}

/// **The neighbouring defect.** `add_field` wrote `{"fields": ...}` over the
/// whole column, so every field added through the admin dropped
/// `title_label`, `published_default` and any other settings key the type
/// carried.
#[test]
fn adding_a_field_keeps_the_types_other_settings_keys() {
    run_test(async {
        let app = shared_app().await;
        seed_scratch_type(app, MERGE_TYPE).await;

        app.state
            .content_types()
            .add_field(MERGE_TYPE, "field_summary", "Summary", "text_long")
            .await
            .expect("adding a new field must succeed");

        let settings: serde_json::Value =
            sqlx::query_scalar("SELECT settings FROM item_type WHERE type = $1")
                .bind(MERGE_TYPE)
                .fetch_one(&app.db)
                .await
                .unwrap();

        assert_eq!(
            settings.get("title_label").and_then(|v| v.as_str()),
            Some("Headline"),
            "adding a field must not drop title_label; settings are now {settings}"
        );
        assert_eq!(
            settings.get("published_default").and_then(|v| v.as_bool()),
            Some(false),
            "adding a field must not drop published_default; settings are now {settings}"
        );
        assert_eq!(
            stored_field_names(app, MERGE_TYPE).await,
            vec!["field_summary".to_string()],
            "the field itself must still have been written"
        );
    });
}

/// **The other neighbouring defect.** An unrecognised `field_type` became a
/// plain `Text` field without a word, so a typo or a stale option silently
/// stored the wrong type.
#[test]
fn an_unknown_field_type_is_refused_rather_than_silently_text() {
    run_test(async {
        let app = shared_app().await;
        seed_scratch_type(app, UNKNOWN_TYPE).await;

        let error = app
            .state
            .content_types()
            .add_field(UNKNOWN_TYPE, "field_mystery", "Mystery", "not_a_field_type")
            .await
            .expect_err("an unknown field type must be refused");

        assert!(
            error.to_string().contains("not_a_field_type"),
            "the error must name the unrecognised type; got {error}"
        );
        assert!(
            stored_field_names(app, UNKNOWN_TYPE).await.is_empty(),
            "a refused add must write nothing"
        );

        // And the route says so too, rather than rendering a server error.
        let cookies = app
            .create_and_login_admin(
                "dupfieldtype",
                "correct-horse-battery-staple",
                "dupfieldtype@test.local",
            )
            .await;
        let (cookies, csrf, build_id) = open_fields_page(app, &cookies, UNKNOWN_TYPE).await;
        let response = post_add_field(
            app,
            &cookies,
            UNKNOWN_TYPE,
            &csrf,
            &build_id,
            "Mystery",
            "field_mystery",
            "not_a_field_type",
        )
        .await;
        let html = body_string(response).await;
        assert!(
            html.contains("not_a_field_type"),
            "the form must name the unrecognised field type; got {html}"
        );
        assert!(
            stored_field_names(app, UNKNOWN_TYPE).await.is_empty(),
            "the refused post must write nothing"
        );
    });
}

/// The shipped migration repairs a row that already holds duplicates, and
/// running it a second time changes nothing.
///
/// This runs the migration's own SQL text, read from the file, so it tests what
/// ships rather than a copy of it.
#[test]
fn the_migration_drops_duplicate_field_definitions_and_is_idempotent() {
    run_test(async {
        let app = shared_app().await;
        const SCRATCH: &str = "dup_field_migration";

        let migration = std::fs::read_to_string(
            common::project_root()
                .join("crates/kernel/migrations/20261006000001_dedupe_item_type_fields.sql"),
        )
        .expect("read the dedupe migration");

        let field = |label: &str, name: &str| {
            serde_json::json!({
                "field_name": name,
                "field_type": "TextLong",
                "label": label,
                "required": false,
                "cardinality": 1,
                "settings": {},
                "personal_data": false,
            })
        };

        // The first copy is the one to keep, so each copy carries a different
        // label: that is how the test can tell which survived.
        let seeded = serde_json::json!({
            "title_label": "Headline",
            "fields": [
                field("Body", "body"),
                field("Dupe first", "field_dupe"),
                field("Dupe second", "field_dupe"),
                field("Dupe third", "field_dupe"),
                field("Tail", "field_tail"),
            ],
        });

        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, 'Dupe Migration', '', true, 'Title', 'test', $2) \
             ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
        )
        .bind(SCRATCH)
        .bind(&seeded)
        .execute(&app.db)
        .await
        .expect("seed a row holding duplicates");

        let read_back = || async {
            let settings: serde_json::Value =
                sqlx::query_scalar("SELECT settings FROM item_type WHERE type = $1")
                    .bind(SCRATCH)
                    .fetch_one(&app.db)
                    .await
                    .unwrap();
            settings
        };

        sqlx::raw_sql(&migration)
            .execute(&app.db)
            .await
            .expect("apply the dedupe migration");
        let once = read_back().await;

        // Idempotent: a second run must leave the row exactly as the first did.
        sqlx::raw_sql(&migration)
            .execute(&app.db)
            .await
            .expect("apply the dedupe migration a second time");
        let twice = read_back().await;

        sqlx::query("DELETE FROM item_type WHERE type = $1")
            .bind(SCRATCH)
            .execute(&app.db)
            .await
            .unwrap();

        let names: Vec<&str> = once
            .get("fields")
            .and_then(|v| v.as_array())
            .expect("fields must still be an array")
            .iter()
            .map(|f| f.get("field_name").and_then(|v| v.as_str()).unwrap_or(""))
            .collect();
        assert_eq!(
            names,
            vec!["body", "field_dupe", "field_tail"],
            "the migration must keep one of each name, in the original order"
        );

        let kept_label = once.get("fields").and_then(|v| v.as_array()).unwrap()[1]
            .get("label")
            .and_then(|v| v.as_str());
        assert_eq!(
            kept_label,
            Some("Dupe first"),
            "the copy kept must be the first one, not a later one"
        );

        assert_eq!(
            once.get("title_label").and_then(|v| v.as_str()),
            Some("Headline"),
            "the migration must not disturb other settings keys"
        );

        assert_eq!(
            twice, once,
            "a second run of the migration must change nothing"
        );
    });
}
