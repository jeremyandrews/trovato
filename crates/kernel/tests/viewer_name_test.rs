#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The signed-in user's account name on every page: `user_name`.
//!
//! `inject_site_context` gives a page template `user_name`, the account name of
//! the signed-in viewer, taken from the user record the viewer was built from.
//! An anonymous visitor gets no `user_name` at all, so a theme tests it with
//! `is defined`. The stock `templates/page.html` prints "Logged in as <name>",
//! escaped.
//!
//! Requires Postgres + Redis.

mod common;

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, user_id_of, username};
use tower_sessions::{MemoryStore, Session};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const PASSWORD: &str = "viewer-name-test-pw-1";

/// A published conference page, deleted again when the test ends.
async fn ordinary_page(app: &TestApp) -> Uuid {
    app.ensure_conference_type().await;
    let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "conference".to_string(),
                title: "Viewer name page".to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("viewer name test".to_string()),
            },
            &admin,
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

async fn page_html(app: &TestApp, path: &str, cookies: &str) -> String {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    assert_eq!(response.status(), StatusCode::OK, "GET {path}");
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// A session holding `user_id`, detached from any cookie.
async fn session_for(user_id: Option<Uuid>) -> Session {
    let session = Session::new(None, Arc::new(MemoryStore::default()), None);
    if let Some(id) = user_id {
        session
            .insert(trovato_kernel::routes::auth::SESSION_USER_ID, id)
            .await
            .unwrap();
    }
    session
}

async fn site_context(app: &TestApp, session: &Session) -> tera::Context {
    let mut context = tera::Context::new();
    trovato_kernel::routes::helpers::inject_site_context(&app.state, session, &mut context, "/")
        .await;
    context
}

/// The context itself: the signed-in viewer's account name under `user_name`,
/// and no `user_name` key for an anonymous visitor.
#[test]
fn the_site_context_carries_the_signed_in_users_name_and_nothing_for_a_visitor() {
    run_test(async {
        let app = shared_app().await;
        let name = username("viewer-name");
        app.create_test_user(&name, PASSWORD, &format!("{name}@example.com"))
            .await;
        let id = user_id_of(app, &name).await;

        let signed_in = site_context(app, &session_for(Some(id)).await).await;
        assert_eq!(
            signed_in.get("user_name").and_then(|v| v.as_str()),
            Some(name.as_str()),
            "a signed-in viewer's page context carries their account name"
        );
        assert_eq!(
            signed_in
                .get("user_authenticated")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        let viewer = signed_in.get("viewer").expect("viewer in context");
        assert!(
            viewer.get("name").is_none(),
            "the name is its own key, not folded into `viewer`"
        );

        let anonymous = site_context(app, &session_for(None).await).await;
        assert!(
            anonymous.get("user_name").is_none(),
            "an anonymous visitor's context has no user_name at all"
        );
    });
}

/// Through the real router and the stock page template: an ordinary content
/// page says who is signed in, and says nothing of the sort to a visitor.
#[test]
fn an_ordinary_page_reads_logged_in_as_the_viewer() {
    run_test(async {
        let app = shared_app().await;
        let page = ordinary_page(app).await;
        let name = username("viewer-name-page");
        let cookies = app
            .create_and_login_user(&name, PASSWORD, &format!("{name}@example.com"))
            .await;

        let html = page_html(app, &format!("/item/{page}"), &cookies).await;
        assert!(
            html.contains(&format!("Logged in as {name}")),
            "the signed-in page names the viewer"
        );

        let anonymous = page_html(app, &format!("/item/{page}"), "").await;
        assert!(
            !anonymous.contains("Logged in as"),
            "an anonymous page names nobody"
        );
    });
}

/// An account name is user input: it renders escaped.
#[test]
fn a_name_holding_markup_renders_escaped() {
    run_test(async {
        let app = shared_app().await;
        let page = ordinary_page(app).await;
        let name = format!("<script>alert(1)</script>{}", Uuid::now_v7().simple());
        let cookies = app
            .create_and_login_user(
                &name,
                PASSWORD,
                &format!("script-{}@example.com", Uuid::now_v7().simple()),
            )
            .await;

        let html = page_html(app, &format!("/item/{page}"), &cookies).await;
        assert!(
            !html.contains("<script>alert(1)</script>"),
            "the raw markup must not reach the page"
        );
        assert!(
            html.contains("Logged in as &lt;script&gt;alert(1)&lt;&#x2F;script&gt;"),
            "the name is printed, escaped"
        );
    });
}
