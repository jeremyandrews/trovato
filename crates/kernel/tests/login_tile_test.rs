#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The login form in a tile (B183): a `user_login` tile shows an anonymous
//! visitor the login page's own form, with a CSRF token issued for that
//! visitor, posting to `/user/login`.
//!
//! The page context only ever minted a token for a signed-in viewer (for the
//! Logout form), and the tile renderer had no session to mint one into, so a
//! site could offer anonymous visitors links to the login page and nothing
//! more. The token is now issued by the tile, when it is drawn: a page
//! without one still starts no session.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, defer_cleanup, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::tile::{CreateTile, Tile};
use uuid::Uuid;

const PASSWORD: &str = "b183-tile-Password-1";

fn stamp() -> String {
    Uuid::now_v7().simple().to_string()
}

/// A page that draws the sidebar: an empty gather's own page, removed when
/// the test ends. (The front page replaces the block the sidebar is in.)
async fn sidebar_page(app: &TestApp) -> String {
    let marker = stamp();
    let query_id = format!("b183_page_{marker}");
    let query: trovato_kernel::gather::GatherQuery = serde_json::from_value(serde_json::json!({
        "query_id": query_id,
        "label": format!("B183 page {marker}"),
        "definition": {
            "base_table": "item",
            "filters": [{
                "exposed": false,
                "field": "title",
                "operator": "contains",
                "value": format!("nosuchtitle{marker}"),
            }],
            "stage_aware": true,
        },
        "display": { "format": "table", "items_per_page": 20 },
        "plugin": "core"
    }))
    .expect("gather deserializes");
    app.state
        .gather()
        .register_query(query)
        .await
        .expect("register gather");
    let state = app.state.clone();
    let cleanup_id = query_id.clone();
    defer_cleanup(move || async move {
        let _ = state.gather().delete_query(&cleanup_id).await;
    });
    format!("/gather/{query_id}")
}

/// A `user_login` tile in the sidebar of `page` only, so no other test's
/// pages change. Removed when the test ends. Returns its machine name.
async fn login_tile(app: &TestApp, page: &str) -> String {
    let machine_name = format!("b183_login_{}", stamp());
    let tile = Tile::create(
        &app.db,
        CreateTile {
            machine_name: machine_name.clone(),
            label: "User login".to_string(),
            region: Some("sidebar".to_string()),
            tile_type: Some("user_login".to_string()),
            config: Some(serde_json::json!({})),
            visibility: Some(serde_json::json!({ "paths": [page] })),
            weight: Some(0),
            status: Some(1),
            plugin: None,
            stage_id: Some(LIVE_STAGE_ID),
        },
    )
    .await
    .expect("create tile");
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        Tile::delete(&pool, tile.id).await.expect("delete tile");
    });
    machine_name
}

/// An account to sign in as, removed (with its sessions) when the test ends.
async fn account(app: &TestApp) -> String {
    let name = common::username("b183tile");
    app.create_test_user(&name, PASSWORD, &format!("{name}@example.com"))
        .await;
    let id = common::user_id_of(app, &name).await;
    let db = app.db.clone();
    defer_cleanup(move || async move {
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&db)
            .await
            .expect("delete the account");
    });
    name
}

struct Page {
    status: StatusCode,
    set_cookie: String,
    location: Option<String>,
    body: String,
}

async fn send(app: &TestApp, request: Request<Body>, cookies: &str) -> Page {
    let response = app.request_with_cookies(request, cookies).await;
    let status = response.status();
    let set_cookie = common::extract_cookies(&response);
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    Page {
        status,
        set_cookie,
        location,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn get(app: &TestApp, path: &str, cookies: &str) -> Page {
    send(
        app,
        Request::get(path).body(Body::empty()).unwrap(),
        cookies,
    )
    .await
}

/// Post the tile's form on `page` with `password`, from the visitor's own
/// rate-limit bucket.
async fn sign_in_through_tile(app: &TestApp, page: &Page, name: &str, password: &str) -> Page {
    let body = format!(
        "username={}&password={}&destination={}&{}",
        urlencoding::encode(name),
        urlencoding::encode(password),
        urlencoding::encode(
            &common::input_value_in(
                common::form_in(&page.body, "user_login").unwrap(),
                "destination"
            )
            .expect("the tile's form returns to its page")
        ),
        common::form_fields(&page.body, "user_login"),
    );
    app.state.lockout().clear_all(name).await.ok();
    send(
        app,
        Request::post("/user/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("x-forwarded-for", common::test_ip_for(name))
            .body(Body::from(body))
            .unwrap(),
        &page.set_cookie,
    )
    .await
}

#[test]
fn an_anonymous_visitor_signs_in_through_the_login_tile() {
    run_test(async {
        let app = shared_app().await;
        let page_path = sidebar_page(app).await;
        let machine = login_tile(app, &page_path).await;
        let name = account(app).await;

        let page = get(app, &page_path, "").await;
        assert_eq!(page.status, StatusCode::OK);
        let tile_at = page
            .body
            .find(&format!(r#"id="tile-{machine}""#))
            .unwrap_or_else(|| panic!("the login tile is drawn:\n{}", page.body));
        let form = common::form_in(&page.body[tile_at..], "user_login")
            .expect("the tile holds the login form");
        assert!(form.contains(r#"action="/user/login""#), "{form}");
        assert!(
            common::input_value_in(form, "_token").is_some_and(|t| !t.is_empty()),
            "the form carries a token: {form}"
        );
        assert!(
            !page.set_cookie.is_empty(),
            "the token lives in a session the page starts"
        );

        let signed_in = sign_in_through_tile(app, &page, &name, PASSWORD).await;
        assert_eq!(
            signed_in.status,
            StatusCode::SEE_OTHER,
            "the token is valid and the sign in succeeds: {}",
            signed_in.body
        );
        assert_eq!(
            signed_in.location.as_deref(),
            Some(page_path.as_str()),
            "and lands back on the page"
        );

        // Signed in, the same page shows no login form.
        let cookies = if signed_in.set_cookie.is_empty() {
            page.set_cookie.clone()
        } else {
            signed_in.set_cookie.clone()
        };
        let after = get(app, &page_path, &cookies).await;
        assert!(
            !after.body.contains(&format!(r#"id="tile-{machine}""#)),
            "a signed-in viewer is not offered the login tile"
        );
        assert!(after.body.contains(r#"action="/user/logout""#), "signed in");
    });
}

#[test]
fn a_page_without_the_login_tile_starts_no_session() {
    run_test(async {
        let app = shared_app().await;
        let with_tile = sidebar_page(app).await;
        login_tile(app, &with_tile).await;
        let without = sidebar_page(app).await;

        let page = get(app, &without, "").await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(
            !page.body.contains(r#"<form id="user_login""#),
            "no login form on a page the tile is not placed on"
        );
        assert!(
            page.set_cookie.is_empty(),
            "an anonymous page without the tile sets no cookie, was {:?}",
            page.set_cookie
        );
    });
}

#[test]
fn a_wrong_password_through_the_tile_lands_on_the_login_page_with_its_error() {
    run_test(async {
        let app = shared_app().await;
        let page_path = sidebar_page(app).await;
        login_tile(app, &page_path).await;
        let name = account(app).await;

        let page = get(app, &page_path, "").await;
        let refused = sign_in_through_tile(app, &page, &name, "not-the-password").await;
        assert_eq!(refused.status, StatusCode::OK, "{}", refused.body);
        assert!(
            refused.body.contains("Invalid username or password"),
            "the login page's own error: {}",
            refused.body
        );
        assert!(
            refused.body.contains(r#"<form id="user_login""#)
                && refused.body.contains(r#"action="/user/login""#),
            "the login page, with its form to try again"
        );
        assert!(
            !refused.body.contains(r#"action="/user/logout""#),
            "nobody is signed in"
        );
    });
}
