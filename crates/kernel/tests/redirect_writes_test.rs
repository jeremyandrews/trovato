#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Redirect writes go through one service now (B266): the admin form, the
//! JSON API and any other caller all reach `RedirectService::create`,
//! `update` and `delete`, which refuse a loop, bump `CacheKind::Redirects`
//! and write an audit entry. Before, `Redirect::create/update/delete` were
//! bare database writes the routes called directly, `detect_loop` had no
//! caller, nothing bumped the cache outside gather's own sync and the B167
//! importer, and nothing was audited.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, defer_cleanup, run_test, shared_app, user_holding};
use trovato_kernel::cache::generation::{CacheKind, current};
use trovato_kernel::services::redirect::Redirect;
use uuid::Uuid;

/// The shared app, with the redirects and audit log plugins' tables in place
/// and enabled.
async fn app() -> &'static TestApp {
    let app = shared_app().await;
    common::ensure_plugin_tables(app, "trovato_redirects");
    app.ensure_plugin_enabled("trovato_redirects").await;
    common::ensure_plugin_tables(app, "trovato_audit_log");
    app.ensure_plugin_enabled("trovato_audit_log").await;
    app
}

/// A path unique to this test run, so parallel tests never collide.
fn unique(prefix: &str) -> String {
    format!("/{prefix}-{}", &Uuid::now_v7().simple().to_string()[20..])
}

/// How many redirects currently have `source`.
async fn count_with_source(app: &TestApp, source: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM redirect WHERE source = $1")
        .bind(source)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// `action`s recorded for one redirect, oldest first.
async fn audit_actions(app: &TestApp, redirect_id: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE entity_type = 'redirect' AND entity_id = $1 \
         ORDER BY created, id",
    )
    .bind(redirect_id.to_string())
    .fetch_all(&app.db)
    .await
    .unwrap()
}

/// Remove a redirect by id when the test ends.
fn forget(app: &TestApp, id: Uuid) {
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM redirect WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
}

// ---------------------------------------------------------------------------
// Loop refusal
// ---------------------------------------------------------------------------

/// A to B, then B to A: the second write completes a loop and is refused,
/// with nothing written.
#[test]
fn a_two_hop_loop_is_refused_with_nothing_written() {
    run_test(async {
        let app = app().await;
        let redirects = app.state.redirects();
        let a = unique("b266-loop-a");
        let b = unique("b266-loop-b");

        let created = redirects.create(&a, &b, 301, "").await.expect("a to b");
        forget(app, created.id);

        let error = redirects
            .create(&b, &a, 301, "")
            .await
            .expect_err("b to a completes the loop");
        assert!(error.to_string().contains("loop"), "{error}");
        assert_eq!(
            count_with_source(app, &b).await,
            0,
            "the refused write left nothing behind"
        );
    });
}

/// A redirect whose destination is its own source is refused outright, with
/// nothing written: zero hops is still a loop.
#[test]
fn a_self_redirect_is_refused_with_nothing_written() {
    run_test(async {
        let app = app().await;
        let redirects = app.state.redirects();
        let c = unique("b266-loop-self");

        let error = redirects
            .create(&c, &c, 301, "")
            .await
            .expect_err("a redirect to itself");
        assert!(error.to_string().contains("loop"), "{error}");
        assert_eq!(count_with_source(app, &c).await, 0);
    });
}

/// An update that would complete a loop is refused the same way a create is,
/// and leaves the existing row as it was.
#[test]
fn an_update_that_would_complete_a_loop_is_refused() {
    run_test(async {
        let app = app().await;
        let redirects = app.state.redirects();
        let a = unique("b266-loop-upd-a");
        let c = unique("b266-loop-upd-c");
        let elsewhere = unique("b266-loop-upd-elsewhere");

        // A -> C, and C -> elsewhere (harmless on its own).
        let a_to_c = redirects.create(&a, &c, 301, "").await.expect("a to c");
        forget(app, a_to_c.id);
        let c_row = redirects
            .create(&c, &elsewhere, 301, "")
            .await
            .expect("c to elsewhere");
        forget(app, c_row.id);

        // Retargeting C at A would close the loop: C -> A -> C.
        let error = redirects
            .update(c_row.id, &c, &a, 301, "")
            .await
            .expect_err("c to a completes the loop");
        assert!(error.to_string().contains("loop"), "{error}");

        let stored = Redirect::find_by_id(&app.db, c_row.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.destination, elsewhere,
            "the refused update changed nothing"
        );
    });
}

// ---------------------------------------------------------------------------
// Cache bump
// ---------------------------------------------------------------------------

/// A write through the service bumps `CacheKind::Redirects`, so another
/// server drops its cached copy without waiting for the TTL.
#[test]
fn a_write_bumps_the_redirects_generation() {
    run_test(async {
        let app = app().await;
        let redirects = app.state.redirects();
        let redis = app.state.redis();
        let source = unique("b266-bump");

        let before = current(redis, CacheKind::Redirects).await.unwrap_or(0);
        let created = redirects
            .create(&source, "/b266-dest", 301, "")
            .await
            .expect("create");
        forget(app, created.id);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if current(redis, CacheKind::Redirects).await.unwrap_or(0) > before {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the create did not bump the redirects generation within 2 s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });
}

// ---------------------------------------------------------------------------
// Audit entries: the admin form and the JSON API both reach the service
// ---------------------------------------------------------------------------

async fn body_text(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A fresh, single-use CSRF token, scraped from the redirect add form's
/// hidden `_token` field.
async fn csrf_token(app: &TestApp, cookies: &str) -> String {
    let response = app
        .request_with_cookies(
            Request::get("/admin/config/redirects/add")
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let html = body_text(response).await;
    let marker = "name=\"_token\" value=\"";
    let start = html.find(marker).expect("csrf token field") + marker.len();
    html[start..].split('"').next().unwrap().to_string()
}

/// The admin form's add submission reaches the service and leaves an audit
/// row, the same as any other write service.
#[test]
fn an_admin_form_write_is_audited() {
    run_test(async {
        let app = app().await;
        let (_, cookies) = user_holding(
            app,
            "b266-form",
            &[
                "access administration pages",
                "view redirects",
                "administer redirects",
            ],
        )
        .await;
        let source = unique("b266-form");
        let token = csrf_token(app, &cookies).await;
        let body = format!(
            "_token={token}&source={}&destination=%2Fb266-form-dest&status_code=301&language=",
            source.replace('/', "%2F")
        );

        let response = app
            .request_with_cookies(
                Request::post("/admin/config/redirects/add")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER, "{response:?}");

        let id: Uuid = sqlx::query_scalar("SELECT id FROM redirect WHERE source = $1")
            .bind(&source)
            .fetch_one(&app.db)
            .await
            .expect("the form write landed");
        forget(app, id);

        assert_eq!(audit_actions(app, id).await, ["create"]);
    });
}

/// The JSON API's create reaches the same service and leaves the same kind
/// of audit row.
#[test]
fn an_api_write_is_audited() {
    run_test(async {
        let app = app().await;
        let (_, cookies) = user_holding(
            app,
            "b266-api",
            &[
                "access administration pages",
                "view redirects",
                "administer redirects",
            ],
        )
        .await;
        let source = unique("b266-api");
        let token = csrf_token(app, &cookies).await;

        let response = app
            .request_with_cookies(
                Request::post("/api/v1/redirect")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("X-CSRF-Token", &token)
                    .body(Body::from(
                        serde_json::json!({
                            "source": source,
                            "destination": "/b266-api-dest",
                            "status_code": 301,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::CREATED, "{response:?}");
        let created: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
        forget(app, id);

        assert_eq!(audit_actions(app, id).await, ["create"]);
    });
}
