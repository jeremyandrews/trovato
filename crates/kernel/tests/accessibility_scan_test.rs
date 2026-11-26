#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The accessibility report also finds images with no text alternative
//! written straight into an item's rich text, not only uploaded files
//! missing `alt_text` (B251).
//!
//! The count is written by the item service on every create and update
//! ([`trovato_kernel::content::accessibility_scan::count_missing_alt`]),
//! stored on `item_accessibility`, and the report only ever reads it back.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for, user_holding};
use serde_json::json;
use trovato_kernel::models::item::{CreateItem, UpdateItem};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const TYPE: &str = "b251_a11y";

async fn ensure_type(app: &TestApp) {
    use trovato_sdk::types::{FieldDefinition, FieldType};
    let fields = vec![FieldDefinition::new("body", FieldType::TextLong).label("Body")];
    let settings = json!({ "fields": serde_json::to_value(&fields).unwrap() });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, $1, '', true, 'Title', 'b251_test', $2) \
         ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
    )
    .bind(TYPE)
    .bind(&settings)
    .execute(&app.db)
    .await
    .unwrap();
    app.state
        .content_types()
        .replace(TYPE, TYPE, None, settings)
        .await
        .ok();
}

fn admin_ctx(uid: Uuid) -> UserContext {
    UserContext::administrator(uid, vec!["administer site".to_string()])
}

async fn create(app: &TestApp, uid: Uuid, title: &str, body_html: &str) -> Uuid {
    create_on_stage(app, uid, title, body_html, None).await
}

async fn create_on_stage(
    app: &TestApp,
    uid: Uuid,
    title: &str,
    body_html: &str,
    stage_id: Option<Uuid>,
) -> Uuid {
    app.state
        .items()
        .create(
            CreateItem {
                item_type: TYPE.to_string(),
                title: title.to_string(),
                author_id: uid,
                status: Some(1),
                promote: None,
                sticky: None,
                fields: Some(json!({
                    "body": {"value": body_html, "format": "filtered_html"},
                })),
                stage_id,
                language: None,
                log: None,
            },
            &admin_ctx(uid),
        )
        .await
        .expect("seed item")
        .id
}

async fn missing_alt_count(app: &TestApp, item_id: Uuid) -> i32 {
    sqlx::query_scalar("SELECT images_missing_alt FROM item_accessibility WHERE item_id = $1")
        .bind(item_id)
        .fetch_one(&app.db)
        .await
        .expect("item_accessibility row")
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", test_ip_for("b251a11y"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        String::from_utf8_lossy(&bytes)
            .replace("&#x2F;", "/")
            .replace("&amp;", "&")
            .replace("&#x3D;", "="),
    )
}

/// An `<img>` with no `alt` attribute at all is a finding.
#[test]
fn an_img_with_no_alt_attribute_is_counted() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let (uid, _) = user_holding(app, "b251create", &["administer site"]).await;
        let id = create(app, uid, "No alt", r#"<p><img src="x"></p>"#).await;
        let pool = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM item WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await;
        });

        assert_eq!(missing_alt_count(app, id).await, 1);
    });
}

/// `alt=""` is a deliberate decorative marker, not a finding.
#[test]
fn an_empty_alt_attribute_is_not_counted() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let (uid, _) = user_holding(app, "b251empty", &["administer site"]).await;
        let id = create(app, uid, "Decorative", r#"<img src="x" alt="">"#).await;
        let pool = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM item WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await;
        });

        assert_eq!(missing_alt_count(app, id).await, 0);
    });
}

/// Editing an item to add real alt text clears its count.
#[test]
fn an_edit_that_adds_alt_text_clears_the_count() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let (uid, _) = user_holding(app, "b251edit", &["administer site"]).await;
        let id = create(app, uid, "Fixed later", r#"<img src="x">"#).await;
        let pool = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM item WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await;
        });
        assert_eq!(missing_alt_count(app, id).await, 1);

        app.state
            .items()
            .update(
                id,
                UpdateItem {
                    title: None,
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: Some(json!({
                        "body": {"value": r#"<img src="x" alt="A cat">"#, "format": "filtered_html"},
                    })),
                    log: None,
                    stage_id: None,
                    author_id: None,
                    created: None,
                },
                &admin_ctx(uid),
            )
            .await
            .expect("update item")
            .expect("item still exists");

        assert_eq!(missing_alt_count(app, id).await, 0);
    });
}

/// The report's "In content" section lists an item only to a viewer allowed
/// to see it: an editor gets an edit link, someone who may neither edit nor
/// view the item (here, an internal stage a published-content fast path
/// never reaches) never sees its row, even though both may open the report.
#[test]
fn the_report_lists_an_item_in_content_only_to_a_viewer_who_may_see_it() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let stage_id = common::create_test_stage(app, "b251stage").await;
        let (uid, _) = user_holding(app, "b251report", &["administer site"]).await;
        let tag = format!("b251tag{}", Uuid::now_v7().simple());
        let id = create_on_stage(app, uid, &tag, r#"<img src="x">"#, Some(stage_id)).await;
        let pool = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM item WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await;
        });
        assert_eq!(missing_alt_count(app, id).await, 1);

        // An editor sees the row, linked to the edit form.
        let (_, editor_cookies) =
            user_holding(app, "b251editor", &["administer site", "edit any content"]).await;
        let (status, html) = get(app, &editor_cookies, "/admin/reports/accessibility").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("In content"), "{html}");
        assert!(
            html.contains(&format!(r#"href="/admin/content/{id}/edit""#)),
            "{html}"
        );
        assert!(html.contains(&tag), "{html}");

        // Someone who may open the report but holds no access to this item's
        // stage never sees its row.
        let (_, plain_cookies) = user_holding(app, "b251plain", &["administer site"]).await;
        let (status, html) = get(app, &plain_cookies, "/admin/reports/accessibility").await;
        assert_eq!(status, StatusCode::OK);
        assert!(!html.contains(&tag), "{html}");
    });
}
