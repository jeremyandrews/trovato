#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Editors read from their stage; everyone else reads Live.
//!
//! Which stages a read draws on was decided in three places, none of which
//! asked a permission: `/admin/stage/switch` stored any string as the session's
//! active stage (for an administrator), search and the alias fallback read that
//! session value for whoever held it, and the gather routes took `?stage=` from
//! anyone, anonymous visitors included. One resolver now decides: Live, or the
//! session's active stage plus Live for a holder of `use stages`
//! (administrators hold it implicitly). A gather's `total` and pager count only
//! the rows the caller can see.
//!
//! Requires Postgres and Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, defer_cleanup, run_test, shared_app, user_holding};
use trovato_kernel::gather::types::GatherQuery;
use trovato_kernel::models::role::KERNEL_PERMISSIONS;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, Item};
use uuid::Uuid;

const USE_STAGES: &str = "use stages";

fn unique() -> String {
    Uuid::now_v7().simple().to_string()[20..].to_string()
}

/// An internal stage, removed with its items when the test ends.
async fn internal_stage(app: &TestApp) -> Uuid {
    let id = common::create_test_stage(app, "b69read").await;
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        for sql in [
            "DELETE FROM url_alias WHERE stage_id = $1",
            "DELETE FROM menu_link WHERE stage_id = $1",
            "DELETE FROM item WHERE stage_id = $1",
            "DELETE FROM stage_config WHERE tag_id = $1",
            "DELETE FROM category_tag WHERE id = $1",
        ] {
            let _ = sqlx::query(sql).bind(id).execute(&pool).await;
        }
    });
    id
}

/// A page item titled `title` on `stage`, with `status`, deleted at the end.
async fn page_on(app: &TestApp, stage: Uuid, title: &str, status: i16) -> Uuid {
    let item = Item::create(
        &app.db,
        CreateItem {
            item_type: "page".to_string(),
            title: title.to_string(),
            author_id: Uuid::nil(),
            status: Some(status),
            promote: None,
            sticky: None,
            fields: None,
            stage_id: Some(stage),
            language: Some("en".to_string()),
            log: None,
        },
    )
    .await
    .expect("create item");
    let pool = app.db.clone();
    let id = item.id;
    defer_cleanup(move || async move {
        let _ = Item::delete(&pool, id).await;
    });
    id
}

async fn body_of(response: axum::response::Response) -> (StatusCode, String) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
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
    body_of(response).await
}

/// A CSRF token for this session, from a page that carries one.
async fn csrf(app: &TestApp, cookies: &str) -> String {
    let (_, html) = get(app, "/user/profile", cookies).await;
    let pos = html.find("csrf-token").expect("csrf-token meta tag");
    let start = html[pos..].find("content=\"").map(|p| pos + p + 9).unwrap();
    let end = html[start..].find('"').map(|p| start + p).unwrap();
    html[start..end].to_string()
}

/// POST `/admin/stage/switch` with `stage_id`, with or without a CSRF token.
async fn switch(
    app: &TestApp,
    cookies: &str,
    stage_id: Option<&str>,
    with_token: bool,
) -> StatusCode {
    let mut request = Request::post("/admin/stage/switch")
        .header("content-type", "application/json")
        .header("x-forwarded-for", common::test_ip_for(cookies));
    if with_token {
        request = request.header("x-csrf-token", csrf(app, cookies).await);
    }
    let body = serde_json::json!({ "stage_id": stage_id }).to_string();
    app.request_with_cookies(request.body(Body::from(body)).unwrap(), cookies)
        .await
        .status()
}

/// Search JSON for `q`: the result ids and the reported total.
async fn search(app: &TestApp, cookies: &str, q: &str) -> (Vec<String>, i64) {
    let (status, body) = get(app, &format!("/api/search?q={q}"), cookies).await;
    assert_eq!(status, StatusCode::OK, "search answers: {body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let ids = json["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["id"].as_str().map(String::from))
        .collect();
    (ids, json["total"].as_i64().unwrap_or(-1))
}

/// A stored, stage-aware gather over pages whose title contains `marker`,
/// `per_page` to a page, with no permission of its own.
async fn gather_for(app: &TestApp, marker: &str, per_page: u32) -> String {
    let query_id = format!("b69read_{}", unique());
    let query: GatherQuery = serde_json::from_value(serde_json::json!({
        "query_id": query_id,
        "label": "B69 stage reads",
        "definition": {
            "base_table": "item",
            "item_type": "page",
            "stage_aware": true,
            "filters": [{ "field": "title", "operator": "contains", "value": marker }],
            "sorts": [{ "field": "title", "direction": "asc", "nulls": null }]
        },
        "display": { "format": "table", "items_per_page": per_page },
        "plugin": "core"
    }))
    .expect("gather deserializes");
    app.state
        .gather()
        .register_query(query)
        .await
        .expect("register gather");
    let pool = app.db.clone();
    let id = query_id.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM gather_query WHERE query_id = $1")
            .bind(&id)
            .execute(&pool)
            .await;
    });
    query_id
}

/// Run a stored gather through the JSON route as `cookies` (empty for an
/// anonymous visitor), with an optional `?stage=` and page.
async fn execute(
    app: &TestApp,
    query_id: &str,
    cookies: &str,
    stage: Option<Uuid>,
    page: u32,
) -> serde_json::Value {
    let mut path = format!("/api/query/{query_id}/execute?page={page}");
    if let Some(stage) = stage {
        path.push_str(&format!("&stage={stage}"));
    }
    let (status, body) = get(app, &path, cookies).await;
    assert_eq!(status, StatusCode::OK, "the gather runs: {body}");
    serde_json::from_str(&body).unwrap()
}

fn row_ids(result: &serde_json::Value) -> Vec<String> {
    result["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["id"].as_str().map(String::from))
        .collect()
}

// =============================================================================
// The permission
// =============================================================================

/// `use stages` is a kernel permission, and neither seeded role holds it.
#[test]
fn use_stages_is_a_kernel_permission_no_seeded_role_holds() {
    run_test(async {
        let app = shared_app().await;
        assert!(
            KERNEL_PERMISSIONS.contains(&USE_STAGES),
            "`use stages` is defined by the kernel"
        );
        let held: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM role_permissions rp JOIN roles r ON r.id = rp.role_id \
             WHERE rp.permission = $1 AND r.name IN ('anonymous user', 'authenticated user')",
        )
        .bind(USE_STAGES)
        .fetch_one(&app.db)
        .await
        .expect("count grants");
        assert_eq!(held, 0, "no seeded role is granted `use stages`");
    });
}

// =============================================================================
// Switching stage
// =============================================================================

/// Without `use stages`, switching and reading the current stage are refused.
#[test]
fn switching_stage_needs_the_permission() {
    run_test(async {
        let app = shared_app().await;
        let stage = internal_stage(app).await;
        let (_, cookies) = user_holding(app, "b69sw", &["access content"]).await;
        assert_eq!(
            switch(app, &cookies, Some(&stage.to_string()), true).await,
            StatusCode::FORBIDDEN,
            "a user without `use stages` may not switch"
        );
        let (status, _) = get(app, "/admin/stage/current", &cookies).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "nor read the current stage");
    });
}

/// A holder who is not an administrator may switch, and reads it back.
#[test]
fn a_holder_switches_and_reads_the_current_stage() {
    run_test(async {
        let app = shared_app().await;
        let stage = internal_stage(app).await;
        let (_, cookies) = user_holding(app, "b69sw", &["access content", USE_STAGES]).await;
        assert_eq!(
            switch(app, &cookies, Some(&stage.to_string()), true).await,
            StatusCode::OK
        );
        let (status, body) = get(app, "/admin/stage/current", &cookies).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains(&stage.to_string()), "{body}");

        assert_eq!(
            switch(app, &cookies, None, true).await,
            StatusCode::OK,
            "back to Live"
        );
    });
}

/// An administrator holds `use stages` without being granted it.
#[test]
fn an_administrator_switches_without_a_grant() {
    run_test(async {
        let app = shared_app().await;
        let stage = internal_stage(app).await;
        let name = common::username("b69swadm");
        let cookies = app
            .create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        assert_eq!(
            switch(app, &cookies, Some(&stage.to_string()), true).await,
            StatusCode::OK
        );
    });
}

/// A stage id that names no stage, or is not an id, is a 400; the switch keeps
/// its CSRF check.
#[test]
fn switching_to_a_stage_that_does_not_exist_is_a_bad_request() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "b69sw", &["access content", USE_STAGES]).await;
        assert_eq!(
            switch(app, &cookies, Some(&Uuid::now_v7().to_string()), true).await,
            StatusCode::BAD_REQUEST,
            "an id no stage has"
        );
        assert_eq!(
            switch(app, &cookies, Some("not-a-stage"), true).await,
            StatusCode::BAD_REQUEST,
            "a value that is not an id"
        );
        let stage = internal_stage(app).await;
        assert_eq!(
            switch(app, &cookies, Some(&stage.to_string()), false).await,
            StatusCode::FORBIDDEN,
            "no CSRF token, no switch"
        );
    });
}

// =============================================================================
// Reading: search
// =============================================================================

/// A holder who switched to a stage finds its items in search; once the
/// permission is gone, the same session reads Live only.
#[test]
fn search_reads_the_active_stage_only_while_the_permission_is_held() {
    run_test(async {
        let app = shared_app().await;
        let stage = internal_stage(app).await;
        let marker = format!("bsr{}", unique());
        let item = page_on(app, stage, &format!("b69 staged {marker}"), 1).await;
        let (user_id, cookies) = user_holding(
            app,
            "b69srch",
            &["access content", "view any content", USE_STAGES],
        )
        .await;

        let (ids, _) = search(app, &cookies, &marker).await;
        assert!(
            !ids.contains(&item.to_string()),
            "before switching, search reads Live"
        );

        assert_eq!(
            switch(app, &cookies, Some(&stage.to_string()), true).await,
            StatusCode::OK
        );
        let (ids, _) = search(app, &cookies, &marker).await;
        assert!(
            ids.contains(&item.to_string()),
            "after switching, the stage's item is found"
        );

        sqlx::query(
            "DELETE FROM role_permissions WHERE permission = $1 \
             AND role_id IN (SELECT role_id FROM user_roles WHERE user_id = $2)",
        )
        .bind(USE_STAGES)
        .bind(user_id)
        .execute(&app.db)
        .await
        .expect("revoke");
        app.state.permissions().invalidate_user(user_id);

        let (ids, total) = search(app, &cookies, &marker).await;
        assert!(
            !ids.contains(&item.to_string()),
            "without the permission the session's stage is not read"
        );
        assert_eq!(total, 0, "and is not counted");
    });
}

// =============================================================================
// Reading: gather
// =============================================================================

/// `?stage=` is ignored without `use stages`: an anonymous visitor and a user
/// who could view the item are both answered from Live, items and total alike.
#[test]
fn a_gathers_stage_parameter_is_ignored_without_the_permission() {
    run_test(async {
        let app = shared_app().await;
        let stage = internal_stage(app).await;
        let marker = format!("bgs{}", unique());
        let item = page_on(app, stage, &format!("b69 staged {marker}"), 1).await;
        let query_id = gather_for(app, &marker, 10).await;

        let anonymous = execute(app, &query_id, "", Some(stage), 1).await;
        assert!(row_ids(&anonymous).is_empty());
        assert_eq!(
            anonymous["total"], 0,
            "an anonymous visitor's total does not count the stage's items"
        );

        let (_, cookies) =
            user_holding(app, "b69gs", &["access content", "view any content"]).await;
        let viewer = execute(app, &query_id, &cookies, Some(stage), 1).await;
        assert!(
            !row_ids(&viewer).contains(&item.to_string()),
            "a user without `use stages` is answered from Live"
        );
        assert_eq!(viewer["total"], 0);

        let (_, cookies) = user_holding(
            app,
            "b69gs",
            &["access content", "view any content", USE_STAGES],
        )
        .await;
        let holder = execute(app, &query_id, &cookies, Some(stage), 1).await;
        assert!(
            row_ids(&holder).contains(&item.to_string()),
            "a holder reads the stage they name"
        );
        assert_eq!(holder["total"], 1);
    });
}

/// A gather's total and pager count only what the caller can see: three
/// published pages and two drafts, two to a page, are three rows on two pages
/// for a viewer who cannot see drafts, and the pages do not overlap.
#[test]
fn a_gathers_total_and_pager_count_only_visible_rows() {
    run_test(async {
        let app = shared_app().await;
        let marker = format!("bgt{}", unique());
        let mut published = Vec::new();
        for n in 0..5 {
            let status = if n % 2 == 0 { 1 } else { 0 };
            let id = page_on(
                app,
                LIVE_STAGE_ID,
                &format!("b69 total {marker} {n}"),
                status,
            )
            .await;
            if status == 1 {
                published.push(id.to_string());
            }
        }
        let query_id = gather_for(app, &marker, 2).await;
        let (_, cookies) = user_holding(app, "b69gt", &["access content"]).await;

        let first = execute(app, &query_id, &cookies, None, 1).await;
        assert_eq!(first["total"], 3, "only the published pages are counted");
        assert_eq!(first["per_page"], 2, "two to a page: two pages");
        assert_eq!(first["page"], 1);
        let second = execute(app, &query_id, &cookies, None, 2).await;
        assert_eq!(second["page"], 2);

        let mut seen = row_ids(&first);
        seen.extend(row_ids(&second));
        let mut sorted = seen.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), seen.len(), "no row is on two pages: {seen:?}");
        let mut expected = published.clone();
        expected.sort();
        assert_eq!(sorted, expected, "every published page, once");
    });
}
