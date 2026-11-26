#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The item form's create and edit paths choose the id and the dates
//! themselves, as they always have (B167).
//!
//! `trovato import` writes items with the ids and dates its file gives. That
//! power was added beside the form paths, not to them; this test passes on
//! `main` from before the importer and must keep passing after it: a form
//! create gets a new UUIDv7 and today's date, an edit adds one revision dated
//! today and keeps the authored on time.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for};
use uuid::Uuid;

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", test_ip_for(cookies))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn revisions_of(app: &TestApp, item: Uuid) -> Vec<(Uuid, i64)> {
    sqlx::query_as("SELECT id, created FROM item_revision WHERE item_id = $1 ORDER BY created, id")
        .bind(item)
        .fetch_all(&app.db)
        .await
        .unwrap()
}

async fn dates_of(app: &TestApp, item: Uuid) -> Option<(i64, i64, String)> {
    sqlx::query_as("SELECT created, changed, title FROM item WHERE id = $1")
        .bind(item)
        .fetch_optional(&app.db)
        .await
        .unwrap()
}

fn forget_item(app: &TestApp, id: Uuid) {
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
}

// =============================================================================
// The form paths, unchanged
// =============================================================================

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

async fn submit(app: &TestApp, cookies: &str, form_uri: &str, extra: &str) -> StatusCode {
    let (_, html) = get(app, cookies, form_uri).await;
    let at = html.find(r#"name="_form_build_id""#).expect("a build id");
    let scoped = &html[html[..at].rfind("<form").unwrap()..];
    let body = format!(
        "_token={}&_form_build_id={}&{extra}",
        urlencoding::encode(&input_value(scoped, "_token").unwrap()),
        urlencoding::encode(&input_value(scoped, "_form_build_id").unwrap()),
    );
    app.request_with_cookies(
        Request::builder()
            .method("POST")
            .uri(form_uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header("x-forwarded-for", test_ip_for(cookies))
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

#[test]
fn an_item_created_and_edited_through_the_forms_is_dated_now_with_a_new_id() {
    run_test(async {
        let app = shared_app().await;
        let name = common::username("b167_form");
        let cookies = app
            .create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let title = format!("B167 form {}", Uuid::now_v7().simple());

        let started = chrono::Utc::now().timestamp();
        let status = submit(
            app,
            &cookies,
            "/item/add/page",
            &format!("title={}&body=hello", urlencoding::encode(&title)),
        )
        .await;
        assert!(status.is_redirection() || status.is_success(), "{status}");
        let (id, created, changed): (Uuid, i64, i64) =
            sqlx::query_as("SELECT id, created, changed FROM item WHERE title = $1")
                .bind(&title)
                .fetch_one(&app.db)
                .await
                .expect("the form created the item");
        forget_item(app, id);
        assert_eq!(
            id.get_version_num(),
            7,
            "a new UUIDv7, chosen by the kernel"
        );
        assert!(created >= started && changed >= started, "dated now");
        let revisions = revisions_of(app, id).await;
        assert_eq!(revisions.len(), 1);
        assert!(revisions[0].1 >= started);
        assert_eq!(revisions[0].0.get_version_num(), 7);

        let edited = format!("{title} edited");
        let status = submit(
            app,
            &cookies,
            &format!("/item/{id}/edit"),
            &format!("title={}&body=again", urlencoding::encode(&edited)),
        )
        .await;
        assert!(status.is_redirection() || status.is_success(), "{status}");
        let (created_after, changed_after, title_after) = dates_of(app, id).await.unwrap();
        assert_eq!(title_after, edited);
        assert_eq!(created_after, created, "an edit keeps the authored on time");
        assert!(changed_after >= started);
        let revisions = revisions_of(app, id).await;
        assert_eq!(revisions.len(), 2, "an edit adds one revision");
        assert!(revisions[1].1 >= started, "dated now");
    });
}
