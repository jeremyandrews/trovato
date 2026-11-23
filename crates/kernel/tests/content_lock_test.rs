//! Content locking end to end on the item edit form (`trovato_content_locking`).
//!
//! The plugin is on by default, and its two routes had no client: nothing ever
//! took a lock, so heartbeat and break had nothing to act on. The edit form now
//! takes the lock, shows another editor's, refuses a save under it, keeps its
//! own alive with the heartbeat, and lets a holder of `break content lock`
//! break it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::{CreateItem, Item};
use uuid::Uuid;

async fn body_of(response: axum::response::Response) -> String {
    String::from_utf8_lossy(&to_bytes(response.into_body(), 4_000_000).await.unwrap()).to_string()
}

/// The edit form's page, as `cookies` sees it.
async fn edit_form(app: &TestApp, item_id: Uuid, cookies: &str) -> String {
    let response = app
        .request_with_cookies(
            Request::get(format!("/admin/content/{item_id}/edit"))
                .header("x-forwarded-for", common::test_ip_for(cookies))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    body_of(response).await
}

/// The page's CSRF token (`<meta name="csrf-token">`).
fn csrf_of(html: &str) -> String {
    let marker = "name=\"csrf-token\" content=\"";
    let start = html.find(marker).expect("a csrf meta tag") + marker.len();
    let end = html[start..].find('"').unwrap() + start;
    html[start..end].to_string()
}

async fn post_json(
    app: &TestApp,
    path: &str,
    cookies: &str,
    csrf: &str,
    body: serde_json::Value,
) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::post(path)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-csrf-token", csrf)
                .header("x-forwarded-for", common::test_ip_for(cookies))
                .body(Body::from(body.to_string()))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_of(response).await)
}

async fn holder(app: &TestApp, item_id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar(
        "SELECT user_id FROM editing_lock WHERE entity_type = 'item' AND entity_id = $1",
    )
    .bind(item_id.to_string())
    .fetch_optional(&app.db)
    .await
    .unwrap()
}

#[test]
fn the_edit_form_takes_shows_keeps_and_breaks_the_lock() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled("trovato_content_locking").await;
        app.ensure_conference_type().await;

        let a_name = common::username("lock_a");
        let b_name = common::username("lock_b");
        let a = app
            .create_and_login_admin(&a_name, "TestPassword123!", &format!("{a_name}@test.com"))
            .await;
        let b = app
            .create_and_login_admin(&b_name, "TestPassword123!", &format!("{b_name}@test.com"))
            .await;
        let a_id = common::user_id_of(app, &a_name).await;
        let b_id = common::user_id_of(app, &b_name).await;

        let item = Item::create(
            &app.db,
            CreateItem {
                item_type: "conference".to_string(),
                title: format!("lock probe {}", Uuid::now_v7().simple()),
                author_id: a_id,
                status: Some(1),
                promote: None,
                sticky: None,
                fields: None,
                stage_id: None,
                language: None,
                log: None,
            },
        )
        .await
        .expect("create item");
        let pool = app.db.clone();
        let item_id = item.id;
        common::defer_cleanup(move || async move {
            let _ = Item::delete(&pool, item_id).await;
        });
        let lock_body = serde_json::json!({"entity_type": "item", "entity_id": item_id});

        // A opens the form: A holds the lock, and the page keeps it alive.
        let a_page = edit_form(app, item_id, &a).await;
        assert_eq!(holder(app, item_id).await, Some(a_id));
        assert!(a_page.contains("data-content-lock "), "the heartbeat hook");
        assert!(!a_page.contains("is editing this item"));

        // B opens it: told A is editing, offered the break.
        let b_page = edit_form(app, item_id, &b).await;
        assert!(b_page.contains("is editing this item"), "{b_page}");
        assert!(b_page.contains(&a_name));
        assert!(b_page.contains("data-content-lock-break"));
        assert_eq!(holder(app, item_id).await, Some(a_id), "B did not take it");

        // B's save under A's lock is refused.
        let (status, body) = post_json(
            app,
            &format!("/item/{item_id}/edit"),
            &b,
            &csrf_of(&b_page),
            serde_json::json!({"title": "B was here"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");

        // A's heartbeat extends it, and each answer carries the next token.
        let (status, body) = post_json(
            app,
            "/api/v1/lock/heartbeat",
            &a,
            &csrf_of(&a_page),
            lock_body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let next: serde_json::Value = serde_json::from_str(&body).unwrap();
        let token = next["csrf_token"]
            .as_str()
            .expect("a fresh token")
            .to_string();
        let (status, body) =
            post_json(app, "/api/v1/lock/heartbeat", &a, &token, lock_body.clone()).await;
        assert_eq!(status, StatusCode::OK, "the returned token works: {body}");

        // B breaks it, and the form is B's.
        let b_page = edit_form(app, item_id, &b).await;
        let (status, body) = post_json(
            app,
            "/api/v1/lock/break",
            &b,
            &csrf_of(&b_page),
            lock_body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let b_page = edit_form(app, item_id, &b).await;
        assert!(!b_page.contains("is editing this item"));
        assert_eq!(holder(app, item_id).await, Some(b_id));

        // Now A's save is the one refused, and B's goes through and lets go.
        let a_page = edit_form(app, item_id, &a).await;
        assert!(a_page.contains("is editing this item"));
        let (status, body) = post_json(
            app,
            &format!("/item/{item_id}/edit"),
            &a,
            &csrf_of(&a_page),
            serde_json::json!({"title": "A was here"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let (status, body) = post_json(
            app,
            &format!("/item/{item_id}/edit"),
            &b,
            &csrf_of(&b_page),
            serde_json::json!({"title": "B saved"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(holder(app, item_id).await, None, "a save releases the lock");
    });
}
