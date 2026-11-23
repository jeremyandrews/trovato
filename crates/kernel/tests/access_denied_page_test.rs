#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The access-denied page, and the pages a refusal used to hide (B156).
//!
//! A browser that is nobody asking for a page that needs a sign in is answered
//! `403` "Access denied" with the login form on the page, and that form lands
//! the visitor back on the page they asked for. It was a `303` to the bare
//! `/user/login`, which lost the page and told a crawler or an old link `200`.
//! Closed registration is the same refusal, where it was `404`. An author with
//! `view own unpublished content` reads their own unpublished item, which
//! answered them `404`; everyone else still gets `404`, so a refusal never
//! confirms that an unpublished item exists.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for, user_holding};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, RegistrationMode, SiteConfig};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const BROWSER: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";

struct Answer {
    status: StatusCode,
    location: Option<String>,
    cookies: String,
    body: String,
}

async fn get(app: &TestApp, path: &str, cookies: &str) -> Answer {
    let request = Request::get(path)
        .header(header::ACCEPT, BROWSER)
        .header("x-forwarded-for", test_ip_for("b156-denied"))
        .body(Body::empty())
        .unwrap();
    answer(app.request_with_cookies(request, cookies).await).await
}

async fn answer(response: axum::response::Response) -> Answer {
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let cookies = common::extract_cookies(&response);
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    Answer {
        status,
        location,
        cookies,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

/// The value of the hidden input `name` on `page`.
fn hidden(page: &str, name: &str) -> Option<String> {
    let marker = format!(r#"name="{name}" value=""#);
    let start = page.find(&marker)? + marker.len();
    let end = page[start..].find('"')?;
    Some(
        page[start..start + end]
            .replace("&amp;", "&")
            .replace("&#x2F;", "/"),
    )
}

/// Delete the user named `name` when the test ends.
fn delete_user_on_exit(app: &TestApp, id: Uuid) {
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&db)
            .await;
    });
}

#[test]
fn nobody_gets_access_denied_with_a_login_form_that_returns_to_the_page() {
    run_test(async {
        let app = shared_app().await;

        for path in ["/admin/structure/types", "/admin", "/item/add/page"] {
            let a = get(app, path, "").await;
            assert_eq!(a.status, StatusCode::FORBIDDEN, "{path}: {}", a.body);
            assert!(
                a.location.is_none(),
                "{path}: no redirect: {:?}",
                a.location
            );
            assert!(a.body.contains("Access denied"), "{path}: {}", a.body);
            assert!(
                a.body
                    .contains("You are not authorized to access this page."),
                "{path}: {}",
                a.body
            );
            assert!(
                a.body.contains(r#"action="/user/login""#),
                "{path}: no login form: {}",
                a.body
            );
            assert_eq!(
                hidden(&a.body, "destination").as_deref(),
                Some(path),
                "{path}: the form returns to the page"
            );
        }

        // A query string is part of where the visitor was going.
        let a = get(app, "/admin/content?status=0", "").await;
        assert_eq!(a.status, StatusCode::FORBIDDEN);
        assert_eq!(
            hidden(&a.body, "destination").as_deref(),
            Some("/admin/content?status=0")
        );

        // A signed-in user who may not is refused without a login form.
        let (id, cookies) = user_holding(app, "b156-denied", &[]).await;
        delete_user_on_exit(app, id);
        let a = get(app, "/admin/structure/types", &cookies).await;
        assert_eq!(a.status, StatusCode::FORBIDDEN, "{}", a.body);
        assert!(!a.body.contains(r#"action="/user/login""#), "{}", a.body);

        // A script is still told 401.
        let response = app
            .request(
                Request::get("/item/add/page")
                    .header("x-forwarded-for", test_ip_for("b156-denied"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    });
}

/// Sign in through the form with `destination`, from a fresh session.
async fn sign_in_to(app: &TestApp, name: &str, destination: &str) -> Answer {
    let path = format!("/user/login?destination={}", urlencode(destination));
    let form = get(app, &path, "").await;
    assert_eq!(form.status, StatusCode::OK, "{}", form.body);
    let token = hidden(&form.body, "_token").expect("csrf token");
    let carried = hidden(&form.body, "destination");
    let body = format!(
        "username={name}&password=test-password-123&_token={token}&destination={}",
        urlencode(carried.as_deref().unwrap_or(destination))
    );
    app.state.lockout().clear_all(name).await.ok();
    let ip = test_ip_for(name);
    app.state.rate_limiter().reset("login", &ip).await.ok();
    let request = Request::post("/user/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ACCEPT, BROWSER)
        .header("x-forwarded-for", ip)
        .body(Body::from(body))
        .unwrap();
    answer(app.request_with_cookies(request, &form.cookies).await).await
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[test]
fn signing_in_lands_on_a_local_destination_and_nowhere_else() {
    run_test(async {
        let app = shared_app().await;
        let name = common::username("b156-dest");
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        delete_user_on_exit(app, common::user_id_of(app, &name).await);

        let a = sign_in_to(app, &name, "/user/profile?tab=1").await;
        assert!(a.status.is_redirection(), "{}: {}", a.status, a.body);
        assert_eq!(a.location.as_deref(), Some("/user/profile?tab=1"));

        for unsafe_destination in [
            "https://evil.example/",
            "//evil.example/",
            "/\\evil.example/",
            "javascript:alert(1)",
        ] {
            let a = sign_in_to(app, &name, unsafe_destination).await;
            assert!(
                a.status.is_redirection(),
                "{unsafe_destination}: {}",
                a.body
            );
            assert_eq!(
                a.location.as_deref(),
                Some("/"),
                "{unsafe_destination} is not followed"
            );
        }
    });
}

#[test]
fn closed_registration_is_access_denied_not_missing() {
    run_test(async {
        let app = shared_app().await;
        let mut saved = Vec::new();
        for key in ["user_registration", "allow_user_registration"] {
            saved.push((key, SiteConfig::get(&app.db, key).await.ok().flatten()));
        }
        let db = app.db.clone();
        common::defer_cleanup(move || async move {
            for (key, value) in saved {
                match value {
                    Some(value) => {
                        let _ = SiteConfig::set(&db, key, value).await;
                    }
                    None => {
                        let _ = SiteConfig::delete(&db, key).await;
                    }
                }
            }
        });
        RegistrationMode::AdminOnly.save(&app.db).await.unwrap();

        let a = get(app, "/user/register", "").await;
        assert_eq!(a.status, StatusCode::FORBIDDEN, "{}", a.body);
        assert!(a.body.contains("Access denied"), "{}", a.body);
        assert!(
            a.body
                .contains("Only administrators may create new accounts"),
            "{}",
            a.body
        );
        assert!(
            a.body.contains(r#"action="/user/login""#),
            "the login form, for a visitor who has an account: {}",
            a.body
        );

        let response = app
            .request(
                Request::post("/user/register/json")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-forwarded-for", test_ip_for("b156-closed"))
                    .body(Body::from(
                        r#"{"username":"b156x","mail":"b156x@example.com","password":"long-enough-pass-1"}"#,
                    ))
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    });
}

async fn unpublished_item_by(app: &TestApp, author: Uuid) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "page".to_string(),
                title: format!("b156 unpublished {}", Uuid::now_v7().simple()),
                author_id: author,
                status: Some(0),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("b156".to_string()),
            },
            &UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]),
        )
        .await
        .expect("create item")
        .id;
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&db)
            .await;
    });
    id
}

#[test]
fn an_author_with_view_own_unpublished_reads_their_own_draft_and_nobody_else_does() {
    run_test(async {
        let app = shared_app().await;
        let (author, author_cookies) = user_holding(
            app,
            "b156-own",
            &["access content", "view own unpublished content"],
        )
        .await;
        delete_user_on_exit(app, author);
        let (plain, plain_cookies) = user_holding(app, "b156-plain", &["access content"]).await;
        delete_user_on_exit(app, plain);
        let (other, other_cookies) = user_holding(
            app,
            "b156-other",
            &["access content", "view own unpublished content"],
        )
        .await;
        delete_user_on_exit(app, other);

        let own = unpublished_item_by(app, author).await;
        let plains = unpublished_item_by(app, plain).await;

        let a = get(app, &format!("/item/{own}"), &author_cookies).await;
        assert_eq!(a.status, StatusCode::OK, "the author reads it: {}", a.body);
        assert!(a.body.contains("b156 unpublished"), "{}", a.body);

        // Without the permission, their own draft stays hidden from its author.
        let a = get(app, &format!("/item/{plains}"), &plain_cookies).await;
        assert_eq!(a.status, StatusCode::NOT_FOUND, "{}", a.body);

        // Another holder of the permission, and nobody, get 404: a refusal
        // never confirms an unpublished item exists.
        let a = get(app, &format!("/item/{own}"), &other_cookies).await;
        assert_eq!(a.status, StatusCode::NOT_FOUND, "{}", a.body);
        let a = get(app, &format!("/item/{own}"), "").await;
        assert_eq!(a.status, StatusCode::NOT_FOUND, "{}", a.body);
    });
}

/// K-error-pages, verified on main: a missing page under a language prefix is
/// the themed 404 with its message, not an empty body.
#[test]
fn a_missing_page_is_the_themed_404_with_a_message() {
    run_test(async {
        let app = shared_app().await;
        for path in [
            "/b156-no-such-page",
            "/item/00000000-0000-0000-0000-00000000b156",
        ] {
            let a = get(app, path, "").await;
            assert_eq!(a.status, StatusCode::NOT_FOUND, "{path}");
            assert!(a.body.contains("error-page--404"), "{path}: {}", a.body);
            assert!(a.body.contains("Page not found"), "{path}: {}", a.body);
            assert!(a.body.contains("</html>"), "{path}: {}", a.body);
        }
    });
}
