#![allow(clippy::unwrap_used, clippy::expect_used)]
//! "Download my data": what the export contains, and whose data it cannot contain.
//!
//! Two claims are worth a test rather than a comment. The document holds the
//! caller's own profile, items, comments and session metadata; and **it cannot
//! contain another account's anything**, which is the failure mode that would turn
//! a privacy feature into a disclosure one.
//!
//! Fixtures carry a UUID suffix, and the rate limiter is keyed on the account, so
//! these pass under default parallelism and again on a database they have already
//! run against.
//!
//! Requires Postgres + Redis (the shared `TestApp`).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use uuid::Uuid;

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// An account with one item and one comment, and its session cookies.
struct Author {
    id: Uuid,
    username: String,
    cookies: String,
    item_title: String,
    comment_body: String,
}

async fn seed_author(app: &TestApp, label: &str) -> Author {
    let username = format!("{label}_{}", Uuid::now_v7().simple());
    app.create_test_user(
        &username,
        "test-password-123",
        &format!("{username}@example.com"),
    )
    .await;
    let cookies = app.login(&username, "test-password-123").await;
    let id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(&username)
        .fetch_one(&app.db)
        .await
        .expect("the account must exist");

    app.ensure_conference_type().await;

    let item_title = format!("Item by {username}");
    let item_id = Uuid::now_v7();
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO item (id, type, title, author_id, status, created, changed, promote, \
         sticky, fields, language) \
         VALUES ($1, 'conference', $2, $3, 1, $4, $4, 0, 0, $5, 'en')",
    )
    .bind(item_id)
    .bind(&item_title)
    .bind(id)
    .bind(now)
    .bind(serde_json::json!({"field_city": "Trento"}))
    .execute(&app.db)
    .await
    .expect("insert item");

    let comment_body = format!("Comment by {username}");
    sqlx::query(
        "INSERT INTO comment (id, item_id, parent_id, author_id, body, body_format, status, \
         created, changed) VALUES ($1, $2, NULL, $3, $4, 'plain_text', 1, $5, $5)",
    )
    .bind(Uuid::now_v7())
    .bind(item_id)
    .bind(id)
    .bind(&comment_body)
    .bind(now)
    .execute(&app.db)
    .await
    .expect("insert comment");

    // A session index entry. The registry is normally written by the session
    // middleware, which needs a ConnectInfo the oneshot test transport does not
    // supply, so the fixture writes it through the registry's own API rather than
    // asserting nothing about sessions at all.
    app.state
        .session_registry()
        .observe(
            id,
            Uuid::now_v7(),
            &format!("session-{}", Uuid::now_v7().simple()),
            "203.0.113.7",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            false,
            now,
        )
        .await
        .expect("index a session");

    Author {
        id,
        username,
        cookies,
        item_title,
        comment_body,
    }
}

async fn cleanup(app: &TestApp, author: &Author) {
    let _ = app
        .state
        .session_registry()
        .revoke_all_except(author.id, None)
        .await;
    let _ = sqlx::query("DELETE FROM comment WHERE author_id = $1")
        .bind(author.id)
        .execute(&app.db)
        .await;
    let _ = sqlx::query("DELETE FROM item WHERE author_id = $1")
        .bind(author.id)
        .execute(&app.db)
        .await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(author.id)
        .execute(&app.db)
        .await;
}

/// Reset the account's export bucket, so a test is never limited by an earlier one.
async fn allow_export(app: &TestApp, user_id: Uuid) {
    let _ = app
        .state
        .rate_limiter()
        .reset("data_export", &user_id.to_string())
        .await;
}

async fn fetch_export(app: &TestApp, author: &Author) -> axum::response::Response {
    allow_export(app, author.id).await;
    app.request_with_cookies(
        Request::get("/user/data-export")
            .body(Body::empty())
            .unwrap(),
        &author.cookies,
    )
    .await
}

/// The document has the shape it promises, and holds the caller's own content.
#[test]
fn the_export_holds_the_accounts_profile_items_and_comments() {
    run_test(async {
        let app = shared_app().await;
        let author = seed_author(app, "exp").await;

        let response = fetch_export(app, &author).await;
        assert_eq!(response.status(), StatusCode::OK);

        let disposition = response
            .headers()
            .get("content-disposition")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            disposition.starts_with("attachment; filename=\""),
            "the export must download rather than render, got: {disposition}"
        );
        assert!(
            disposition.contains("trovato-data-"),
            "the filename must be recognizable, got: {disposition}"
        );
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-store, private"),
            "a personal document must not be cached"
        );

        let body = body_text(response).await;
        let export: serde_json::Value =
            serde_json::from_str(&body).expect("the export must be JSON");

        assert_eq!(export["export_format"], "trovato-data-export-1");
        assert_eq!(export["account"]["username"], author.username);
        assert_eq!(export["account"]["id"], author.id.to_string());
        assert!(
            export["site"]["exported_at"].is_i64(),
            "the export must say when it was made"
        );

        let items = export["items"].as_array().expect("items must be an array");
        assert_eq!(items.len(), 1, "the one authored item must be present");
        assert_eq!(items[0]["title"], author.item_title);
        assert_eq!(
            items[0]["fields"]["field_city"], "Trento",
            "an item's fields must be exported, not just its title"
        );

        let comments = export["comments"]
            .as_array()
            .expect("comments must be an array");
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0]["body"], author.comment_body);
        assert_eq!(
            comments[0]["status"], "Published",
            "a comment's status must be the label, not the raw number"
        );

        // Session metadata, and only metadata. The token is what would let whoever
        // reads the file be that session, so its absence is the assertion that
        // matters.
        let sessions = export["sessions"]
            .as_array()
            .expect("sessions must be an array");
        assert!(
            !sessions.is_empty(),
            "the indexed session must appear in the export"
        );
        for session in sessions {
            assert!(session["last_seen"].is_i64());
            assert!(session["created_at"].is_i64());
            assert!(
                session["device_name"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("Chrome"),
                "the device name must be readable, got: {session}"
            );
            assert!(
                session.get("session_id").is_none() && session.get("token").is_none(),
                "no session token may appear in the export: {session}"
            );
        }

        assert!(
            !body.contains("203.0.113.7"),
            "a session's IP is the rate limiter's business, not the export's"
        );

        // And the file says what it does not contain.
        let not_included = export["not_included"]
            .as_array()
            .expect("not_included must be an array");
        assert!(
            not_included
                .iter()
                .any(|s| s.as_str().unwrap_or_default().contains("reading history")),
            "the file must say viewed content is absent"
        );

        cleanup(app, &author).await;
    });
}

/// **Another account's data cannot appear.** The one failure that would make this a
/// disclosure feature rather than a privacy one.
#[test]
fn one_accounts_export_contains_nothing_of_anothers() {
    run_test(async {
        let app = shared_app().await;
        let mine = seed_author(app, "expmine").await;
        let theirs = seed_author(app, "exptheirs").await;

        let body = body_text(fetch_export(app, &mine).await).await;

        assert!(
            body.contains(&mine.item_title),
            "my own item must be in my export"
        );
        for foreign in [
            theirs.username.as_str(),
            theirs.item_title.as_str(),
            theirs.comment_body.as_str(),
            &theirs.id.to_string(),
        ] {
            assert!(
                !body.contains(foreign),
                "another account's data must not appear in my export: {foreign}"
            );
        }

        // And symmetrically, so the test is not passing by accident of ordering.
        let their_body = body_text(fetch_export(app, &theirs).await).await;
        assert!(their_body.contains(&theirs.item_title));
        assert!(!their_body.contains(&mine.item_title));

        cleanup(app, &mine).await;
        cleanup(app, &theirs).await;
    });
}

/// A second download inside the hour is refused, with a Retry-After.
#[test]
fn a_second_export_inside_the_hour_is_refused() {
    run_test(async {
        let app = shared_app().await;
        let author = seed_author(app, "explimit").await;

        let first = fetch_export(app, &author).await;
        assert_eq!(first.status(), StatusCode::OK);

        // Deliberately not resetting the bucket this time.
        let second = app
            .request_with_cookies(
                Request::get("/user/data-export")
                    .body(Body::empty())
                    .unwrap(),
                &author.cookies,
            )
            .await;
        assert_eq!(
            second.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "the second download inside the hour must be refused"
        );
        assert!(
            second.headers().contains_key("retry-after"),
            "a refusal must say when to come back"
        );

        cleanup(app, &author).await;
    });
}

/// An anonymous visitor is sent to log in rather than handed a document.
#[test]
fn an_anonymous_visitor_cannot_download_an_export() {
    run_test(async {
        let app = shared_app().await;

        let response = app
            .request(
                Request::get("/user/data-export")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(
            location.contains("/user/login"),
            "expected a login redirect, got {location}"
        );
    });
}

/// The profile page offers the download and says what is not in it.
#[test]
fn the_profile_page_offers_the_download_and_states_the_exclusions() {
    run_test(async {
        let app = shared_app().await;
        let author = seed_author(app, "expprofile").await;

        let response = app
            .request_with_cookies(
                Request::get("/user/profile").body(Body::empty()).unwrap(),
                &author.cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;

        assert!(
            html.contains("/user/data-export"),
            "the profile page must link to the export"
        );
        assert!(
            html.contains("Not included:") && html.contains("reading history"),
            "the page must say what is not in the export, got: {html}"
        );
        assert!(
            html.contains("One download per hour"),
            "the page must say the limit before someone hits it"
        );

        cleanup(app, &author).await;
    });
}
