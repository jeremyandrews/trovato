#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The revision history page (B158, K-revisions).
//!
//! The list marked no current revision and offered no way to delete a past
//! one, and only a user who could edit the item could read it, so a role
//! that holds Drupal's `view revisions` without edit access was refused. The
//! list now says "Current revision" on the item's current row, a holder of
//! `delete revisions` with delete access deletes any other row, and `view
//! revisions` opens the history to a user who may view the item.
//!
//! These drive the real router and read the rows.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, UpdateItem};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const TYPE: &str = "b158_revisions";
/// The two permissions, by name, as a role grants them.
const VIEW_REVISIONS: &str = "view revisions";
const DELETE_REVISIONS: &str = "delete revisions";

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

async fn ensure_type(app: &TestApp) {
    let settings = serde_json::json!({ "fields": [] });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'B158 Revisions', '', true, 'Title', 'b158_test', $2) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(TYPE)
    .bind(&settings)
    .execute(&app.db)
    .await
    .unwrap();
    app.state
        .content_types()
        .replace(TYPE, "B158 Revisions", None, settings)
        .await
        .ok();
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn post_form(app: &TestApp, cookies: &str, uri: &str, body: String) -> StatusCode {
    app.request_with_cookies(
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

/// The first form token on `html` after `marker`, or the first on the page
/// (the layout's own). Any token the session issued is accepted.
fn token_after(html: &str, marker: &str) -> String {
    const NEEDLE: &str = r#"name="_token" value=""#;
    let after = html
        .find(marker)
        .map(|at| &html[at..])
        .filter(|rest| rest.contains(NEEDLE));
    let rest = after.unwrap_or(html);
    let name = rest.find(NEEDLE).expect("a token") + NEEDLE.len();
    let end = rest[name..].find('"').unwrap() + name;
    rest[name..end].to_string()
}

/// A form token this session issued, from the logout form every site page
/// carries for a signed in user.
async fn session_token(app: &TestApp, cookies: &str) -> String {
    let (_, html) = get(app, cookies, "/").await;
    token_after(&html, "/user/logout")
}

fn service_admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec![])
}

/// A published item with three revisions, newest last in the returned list.
async fn seed_item(app: &TestApp) -> (Uuid, Vec<Uuid>) {
    let item = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: TYPE.to_string(),
                title: "History probe one".to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: None,
                sticky: None,
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: None,
            },
            &service_admin(),
        )
        .await
        .expect("seed item");
    let id = item.id;
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    for title in ["History probe two", "History probe three"] {
        app.state
            .items()
            .update(
                id,
                UpdateItem {
                    title: Some(title.to_string()),
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: None,
                    log: Some(format!("to {title}")),
                    stage_id: None,
                    author_id: None,
                    created: None,
                },
                &service_admin(),
            )
            .await
            .expect("update")
            .expect("item");
    }
    let revisions: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM item_revision WHERE item_id = $1 ORDER BY created, id")
            .bind(id)
            .fetch_all(&app.db)
            .await
            .unwrap();
    assert_eq!(revisions.len(), 3);
    (id, revisions)
}

async fn revision_exists(app: &TestApp, id: Uuid) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM item_revision WHERE id = $1)")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// **The finding.** The list names the current revision, and an
/// administrator deletes a past one; the current one is refused.
#[test]
fn the_list_marks_the_current_revision_and_deletes_a_past_one() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let name = common::username("b158revadm");
        let cookies = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple",
                &format!("{name}@test.local"),
            )
            .await;
        let (id, revisions) = seed_item(app).await;
        let current = revisions[2];
        let oldest = revisions[0];

        let (status, html) = get(app, &cookies, &format!("/item/{id}/revisions")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            html.matches("Current revision").count(),
            1,
            "one row is marked current"
        );
        let delete_oldest = format!("/item/{id}/revisions/{oldest}/delete");
        assert!(html.contains(&delete_oldest), "a past row offers Delete");
        assert!(
            !html.contains(&format!("/item/{id}/revisions/{current}/delete")),
            "the current row offers no Delete"
        );

        let token = token_after(&html, &delete_oldest);
        let status = post_form(
            app,
            &cookies,
            &delete_oldest,
            format!("_token={}", urlencoding::encode(&token)),
        )
        .await;
        assert!(status.is_redirection(), "delete refused: {status}");
        assert!(!revision_exists(app, oldest).await, "the revision is gone");

        // The current revision stays, whatever is posted.
        let token = session_token(app, &cookies).await;
        let status = post_form(
            app,
            &cookies,
            &format!("/item/{id}/revisions/{current}/delete"),
            format!("_token={}", urlencoding::encode(&token)),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            revision_exists(app, current).await,
            "the current revision stays"
        );
    });
}

/// `view revisions` opens the history to a user who may view the item but not
/// edit it; without it they are refused, and neither offers Revert or Delete.
#[test]
fn view_revisions_opens_the_history_without_edit_access() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let (id, _) = seed_item(app).await;

        let (_, reader) =
            common::user_holding(app, "b158revreader", &["access content", VIEW_REVISIONS]).await;
        let (status, html) = get(app, &reader, &format!("/item/{id}/revisions")).await;
        assert_eq!(status, StatusCode::OK, "view revisions reads the history");
        assert!(html.contains("History probe one"));
        assert!(!html.contains("/revert/"), "no Revert without edit access");
        assert!(
            !html.contains("/delete\""),
            "no Delete without delete revisions"
        );

        let (_, plain) = common::user_holding(app, "b158revplain", &["access content"]).await;
        let (status, _) = get(app, &plain, &format!("/item/{id}/revisions")).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "no history without the permission"
        );
    });
}

/// Deleting a revision needs `delete revisions` and delete access to the item:
/// either alone is refused and the row stays.
#[test]
fn deleting_a_revision_needs_delete_revisions_and_delete_access() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let (id, revisions) = seed_item(app).await;
        let oldest = revisions[0];
        let path = format!("/item/{id}/revisions/{oldest}/delete");

        for (prefix, permissions) in [
            (
                "b158revnodel",
                vec!["access content", VIEW_REVISIONS, "delete any content"],
            ),
            (
                "b158revnoitem",
                vec!["access content", VIEW_REVISIONS, DELETE_REVISIONS],
            ),
        ] {
            let (_, cookies) = common::user_holding(app, prefix, &permissions).await;
            let (_, html) = get(app, &cookies, &format!("/item/{id}/revisions")).await;
            assert!(!html.contains(&path), "{prefix}: Delete offered");
            let token = session_token(app, &cookies).await;
            let status = post_form(
                app,
                &cookies,
                &path,
                format!("_token={}", urlencoding::encode(&token)),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{prefix}");
            assert!(revision_exists(app, oldest).await, "{prefix}: deleted");
        }

        let (_, cookies) = common::user_holding(
            app,
            "b158revdel",
            &[
                "access content",
                VIEW_REVISIONS,
                DELETE_REVISIONS,
                "delete any content",
            ],
        )
        .await;
        let (_, html) = get(app, &cookies, &format!("/item/{id}/revisions")).await;
        assert!(html.contains(&path), "Delete offered");
        let token = token_after(&html, &path);
        let status = post_form(
            app,
            &cookies,
            &path,
            format!("_token={}", urlencoding::encode(&token)),
        )
        .await;
        assert!(status.is_redirection(), "delete refused: {status}");
        assert!(!revision_exists(app, oldest).await);
    });
}
