#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The item page names its author (B166).
//!
//! `view_item` put `item` (carrying only `author_id`), `viewer` and the rest
//! in the template context, but never a name — so a theme had to hardcode
//! "by Jeremy" or print nothing at all. The item page now resolves the
//! author through `UserService::author_views`, the same cached lookup
//! `/api/item`'s `include=author` already uses, and puts `author.name` (and
//! `author.url`, only when the viewer may see that profile) in the context.
//! The shipped item template reads it as `{% if author %}by {{ author.name }}{% endif %}`.
//!
//! A deleted account's content is reattributed to the kernel's anonymous
//! sentinel (`ANONYMOUS_USER_ID`), which has always had its own row
//! (`20260212000001_create_users.sql`) named `anonymous` — so this never
//! produces an empty name or a 500, and the sentinel itself never gets a
//! profile link, permission or not.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, defer_cleanup, run_test, shared_app, user_holding};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::user::ANONYMOUS_USER_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Advisory-lock key guarding this file's item-type seeding.
const TYPE_SEED_LOCK: i64 = 0x_1B66_0000_0001;

const ITEM_TYPE: &str = "b166_item_author_test";

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

async fn ensure_item_type(app: &TestApp) {
    let mut tx = app.db.begin().await.expect("begin type seed");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(TYPE_SEED_LOCK)
        .execute(&mut *tx)
        .await
        .expect("take type seed lock");

    let settings = serde_json::json!({ "fields": [] });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'B166 Item Author Test', 'Fixture type', true, 'Title', 'core', $2) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ITEM_TYPE)
    .bind(&settings)
    .execute(&mut *tx)
    .await
    .expect("seed item type");
    tx.commit().await.expect("commit type seed");

    app.state
        .content_types()
        .replace(
            ITEM_TYPE,
            "B166 Item Author Test",
            Some("Fixture type"),
            settings,
        )
        .await
        .ok();
}

/// A published item by `author_id`, removed (with its alias) when the test ends.
async fn item_by(app: &TestApp, author_id: Uuid, title: &str) -> Uuid {
    ensure_item_type(app).await;
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: ITEM_TYPE.to_string(),
                title: title.to_string(),
                author_id,
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: None,
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id;
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(format!("/item/{id}"))
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

async fn get(app: &TestApp, cookies: &str, path: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The problem this fixes: an item page named nobody, so a theme had to
/// hardcode a name or print the id.
#[test]
fn the_item_page_prints_the_authors_display_name() {
    run_test(async {
        let app = shared_app().await;
        let (author_id, _cookies) = user_holding(app, "b166name", &[]).await;
        let name: String = sqlx::query_scalar("SELECT name FROM users WHERE id = $1")
            .bind(author_id)
            .fetch_one(&app.db)
            .await
            .expect("author name");
        let id = item_by(app, author_id, &format!("B166 named {name}")).await;

        let (status, html) = get(app, "", &format!("/item/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{html}");
        assert!(
            html.contains(&format!("item__author\">by {name}<")),
            "got {html}"
        );
    });
}

/// No `access user profiles`: the name prints, but it is not a link — neither
/// for an anonymous visitor nor a signed-in viewer without the permission.
#[test]
fn the_item_page_omits_the_profile_link_without_the_permission() {
    run_test(async {
        let app = shared_app().await;
        let (author_id, _) = user_holding(app, "b166nolink", &[]).await;
        let id = item_by(app, author_id, "B166 no link").await;
        let needle = format!("href=\"/user/{author_id}\"");

        let (status, as_anon) = get(app, "", &format!("/item/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{as_anon}");
        assert!(!as_anon.contains(&needle), "got {as_anon}");

        let (_, viewer_cookies) = user_holding(app, "b166viewer", &[]).await;
        let (status, as_viewer) = get(app, &viewer_cookies, &format!("/item/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{as_viewer}");
        assert!(!as_viewer.contains(&needle), "got {as_viewer}");
    });
}

/// `access user profiles`: the name becomes a link to `/user/{author_id}`.
#[test]
fn the_item_page_links_the_profile_when_the_viewer_may_see_it() {
    run_test(async {
        let app = shared_app().await;
        let (author_id, _) = user_holding(app, "b166linked", &[]).await;
        let id = item_by(app, author_id, "B166 linked").await;

        let (_, viewer_cookies) = user_holding(app, "b166canview", &["access user profiles"]).await;
        let (status, html) = get(app, &viewer_cookies, &format!("/item/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{html}");
        assert!(
            html.contains(&format!("href=\"/user/{author_id}\"")),
            "got {html}"
        );
    });
}

/// A deleted (or never-attributed) author: the kernel's anonymous display
/// name, a 200 rather than a 500, and never a profile link — even for a
/// viewer who may otherwise see one, since there is no profile to send them
/// to.
#[test]
fn a_deleted_authors_item_shows_the_anonymous_name_and_never_links() {
    run_test(async {
        let app = shared_app().await;
        let id = item_by(app, ANONYMOUS_USER_ID, "B166 deleted author").await;

        let (_, viewer_cookies) =
            user_holding(app, "b166candeleted", &["access user profiles"]).await;
        let (status, html) = get(app, &viewer_cookies, &format!("/item/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{html}");
        assert!(html.contains("item__author\">by anonymous<"), "got {html}");
        assert!(
            !html.contains(&format!("href=\"/user/{ANONYMOUS_USER_ID}\"")),
            "the sentinel account must never be linked: {html}"
        );
    });
}

/// No email, login-name distinction, or any other account field reaches the
/// page — only what `AuthorView` carries.
#[test]
fn the_item_page_never_prints_the_authors_email() {
    run_test(async {
        let app = shared_app().await;
        let name = format!("b166mail-{}", Uuid::now_v7().simple());
        let mail = format!("{name}@example.com");
        app.create_test_user(&name, "test-password-123", &mail)
            .await;
        let author_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(&name)
            .fetch_one(&app.db)
            .await
            .expect("author id");
        let id = item_by(app, author_id, "B166 private fields").await;

        let (status, html) = get(app, "", &format!("/item/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{html}");
        assert!(!html.contains(&mail), "got {html}");
    });
}
