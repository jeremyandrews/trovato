#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B248: the search total and pager count exactly what the viewer is shown.
//!
//! `/api/v1/search` and `/search` took `total` from one SQL count and then
//! dropped rows from the page with `check_access`. A viewer could read "3
//! results" with 2 listed, or a short page with a pager offering more, which
//! told them a hidden item matched. Each test here makes items the viewer may
//! and may not see, walks every page through the real route, and asserts:
//! `total` equals the rows summed across all pages, no page is short except
//! the last, and the last page offers no next page.
//!
//! The case of a role holding `search content` without `access content` is in
//! `search_query_test.rs`, beside the other test that edits the anonymous
//! role.
//!
//! Requires Postgres and Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, defer_cleanup, run_test, shared_app, user_holding};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const PER_PAGE: u64 = 2;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// A word no other item in the shared database holds.
fn unique_term(prefix: &str) -> String {
    let id = Uuid::now_v7().simple().to_string();
    format!("{prefix}{}", &id[id.len() - 12..])
}

/// A page item matching `term` by its title, deleted when the test ends.
///
/// Not a conference: `tutorial_test` asserts every published conference is
/// on Live, and these items sit on internal stages while a test runs.
async fn item(app: &TestApp, term: &str, author: Uuid, status: i16, stage: Uuid) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "page".to_string(),
                title: format!("{term} {}", Uuid::now_v7().simple()),
                author_id: author,
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: None,
                stage_id: Some(stage),
                language: Some("en".to_string()),
                log: Some("b248 search pager test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create")
        .id;
    let items = std::sync::Arc::clone(app.state.items());
    defer_cleanup(move || async move {
        let _ = items.delete(id, &admin()).await;
    });
    id
}

/// An internal stage (the column default), removed with its items at the end.
async fn internal_stage(app: &TestApp) -> Uuid {
    let id = common::create_test_stage(app, "b248int").await;
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        for sql in [
            "DELETE FROM item WHERE stage_id = $1",
            "DELETE FROM stage_config WHERE tag_id = $1",
            "DELETE FROM category_tag WHERE id = $1",
        ] {
            let _ = sqlx::query(sql).bind(id).execute(&pool).await;
        }
    });
    id
}

async fn get(app: &TestApp, path: &str, cookies: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(path)
                .header("x-forwarded-for", common::test_ip_for(cookies))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Switch the session to `stage` through the real route, CSRF token included.
async fn switch_stage(app: &TestApp, cookies: &str, stage: Uuid) {
    let (_, html) = get(app, "/user/profile", cookies).await;
    let pos = html.find("csrf-token").expect("csrf-token meta tag");
    let start = html[pos..].find("content=\"").map(|p| pos + p + 9).unwrap();
    let end = html[start..].find('"').map(|p| start + p).unwrap();
    let token = &html[start..end];
    let response = app
        .request_with_cookies(
            Request::post("/admin/stage/switch")
                .header("content-type", "application/json")
                .header("x-forwarded-for", common::test_ip_for(cookies))
                .header("x-csrf-token", token)
                .body(Body::from(
                    serde_json::json!({ "stage_id": stage.to_string() }).to_string(),
                ))
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK, "stage switch");
}

/// Walk every page of `/api/v1/search?q={term}` as `cookies` and check the
/// pager against what the pages hold. Returns the ids shown.
async fn walk_pages(app: &TestApp, term: &str, cookies: &str, expected: usize) -> Vec<String> {
    let mut shown = Vec::new();
    let mut page = 1u64;
    let mut totals = Vec::new();
    loop {
        let (status, body) = get(
            app,
            &format!("/api/v1/search?q={term}&page={page}&per_page={PER_PAGE}"),
            cookies,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "page {page}: {body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        let total = json["total"].as_u64().unwrap();
        totals.push(total);
        let rows: Vec<String> = json["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect();
        let has_next = page * PER_PAGE < total;
        if has_next {
            assert_eq!(
                rows.len() as u64,
                PER_PAGE,
                "page {page} of total {total} is short but the pager offers a next page"
            );
        }
        shown.extend(rows);
        if !has_next || page > 20 {
            break;
        }
        page += 1;
    }
    assert!(
        totals.windows(2).all(|w| w[0] == w[1]),
        "total changed between pages: {totals:?}"
    );
    let total = totals[0];
    assert_eq!(
        total,
        shown.len() as u64,
        "total {total} but {} rows across all pages",
        shown.len()
    );
    assert_eq!(
        shown.len(),
        expected,
        "the viewer is shown every item they may see"
    );

    // The HTML page reports the same count.
    let (status, html) = get(app, &format!("/search?q={term}"), cookies).await;
    assert_eq!(status, StatusCode::OK);
    let found = format!("Found {total} result");
    assert!(html.contains(&found), "the search page says {found:?}");
    shown
}

/// An anonymous visitor holding `search content` and `access content`: other
/// authors' unpublished items match but are never counted.
#[test]
fn anonymous_total_counts_only_published_items() {
    run_test(async {
        let app = shared_app().await;
        let term = unique_term("zqb248anon");
        let (author, _) = user_holding(app, "b248anonauthor", &[]).await;
        for _ in 0..3 {
            item(app, &term, author, 1, LIVE_STAGE_ID).await;
        }
        for _ in 0..2 {
            item(app, &term, author, 0, LIVE_STAGE_ID).await;
        }
        walk_pages(app, &term, "", 3).await;
    });
}

/// An author's own unpublished items without the permission that lets them
/// view one: not counted, where they used to be counted and then dropped.
#[test]
fn own_unpublished_items_without_the_permission_are_not_counted() {
    run_test(async {
        let app = shared_app().await;
        let term = unique_term("zqb248ownno");
        let (author, cookies) = user_holding(app, "b248ownno", &[]).await;
        for _ in 0..3 {
            item(app, &term, Uuid::nil(), 1, LIVE_STAGE_ID).await;
        }
        for _ in 0..2 {
            item(app, &term, author, 0, LIVE_STAGE_ID).await;
        }
        walk_pages(app, &term, &cookies, 3).await;
    });
}

/// With `view own unpublished content` the author's own drafts are counted
/// and shown; another author's draft still is not.
#[test]
fn own_unpublished_items_with_the_permission_are_counted_and_shown() {
    run_test(async {
        let app = shared_app().await;
        let term = unique_term("zqb248ownyes");
        let (author, cookies) =
            user_holding(app, "b248ownyes", &["view own unpublished content"]).await;
        let (other, _) = user_holding(app, "b248ownother", &[]).await;
        item(app, &term, Uuid::nil(), 1, LIVE_STAGE_ID).await;
        let mut drafts = Vec::new();
        for _ in 0..2 {
            drafts.push(item(app, &term, author, 0, LIVE_STAGE_ID).await);
        }
        item(app, &term, other, 0, LIVE_STAGE_ID).await;
        let shown = walk_pages(app, &term, &cookies, 3).await;
        for draft in drafts {
            assert!(shown.contains(&draft.to_string()), "own draft is listed");
        }
    });
}

/// A `use stages` editor reading an internal stage: items on it are left out
/// of search for anyone but an administrator, so they are not counted either.
#[test]
fn internal_stage_items_are_not_counted_for_a_non_administrator() {
    run_test(async {
        let app = shared_app().await;
        let term = unique_term("zqb248int");
        let stage = internal_stage(app).await;
        let (_, cookies) = user_holding(app, "b248inted", &["use stages"]).await;
        for _ in 0..3 {
            item(app, &term, Uuid::nil(), 1, LIVE_STAGE_ID).await;
        }
        for _ in 0..2 {
            item(app, &term, Uuid::nil(), 1, stage).await;
        }
        switch_stage(app, &cookies, stage).await;
        walk_pages(app, &term, &cookies, 3).await;
    });
}

/// An administrator is shown and counted every match: any status, any author,
/// the internal stage they selected included.
#[test]
fn administrator_total_counts_every_match() {
    run_test(async {
        let app = shared_app().await;
        let term = unique_term("zqb248admin");
        let stage = internal_stage(app).await;
        let name = common::username("b248admin");
        let cookies = app
            .create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let (other, _) = user_holding(app, "b248adminother", &[]).await;
        item(app, &term, Uuid::nil(), 1, LIVE_STAGE_ID).await;
        item(app, &term, Uuid::nil(), 1, LIVE_STAGE_ID).await;
        item(app, &term, other, 0, LIVE_STAGE_ID).await;
        item(app, &term, Uuid::nil(), 1, stage).await;
        item(app, &term, Uuid::nil(), 0, stage).await;
        switch_stage(app, &cookies, stage).await;
        walk_pages(app, &term, &cookies, 5).await;
    });
}
