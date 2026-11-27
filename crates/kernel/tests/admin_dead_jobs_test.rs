#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The dead jobs admin page: `/admin/reports/dead-jobs` (B252).
//!
//! Exercises the real HTTP router: only `administer site` may open the page;
//! Retry puts a dead job back in its queue; Discard asks first, through the
//! shared confirmation layer, and only then deletes; a forged discard token
//! never reaches the confirmation page; the dashboard links the page to
//! whoever may open it.
//!
//! Every row a test makes carries a fresh tag (the plugin name), so a shared
//! database does not interfere, and is deleted when the test ends. Requires
//! Postgres + Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, defer_cleanup, run_test, shared_app, test_ip_for};
use sqlx::Row;
use uuid::Uuid;

const PASSWORD: &str = "correct-horse-battery-staple";

/// A fresh tag, used as the dead job's `plugin_name` so a filter isolates it.
fn tag() -> String {
    format!("djtest{}", Uuid::now_v7().simple())
}

fn encode(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", test_ip_for("b252-dead-jobs"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// POST a form; returns the status, the `Location` and the body.
async fn post(
    app: &TestApp,
    cookies: &str,
    uri: &str,
    form: String,
) -> (StatusCode, String, String) {
    let response = app
        .request_with_cookies(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::REFERER, "http://localhost/admin/reports/dead-jobs")
                .header("x-forwarded-for", test_ip_for("b252-dead-jobs"))
                .body(Body::from(form))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        location,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn token(html: &str) -> String {
    let needle = r#"name="_token" value=""#;
    let at = html.find(needle).expect("a form token") + needle.len();
    html[at..at + html[at..].find('"').unwrap()].to_string()
}

/// An administrator's session, torn down when the test ends.
async fn admin(app: &TestApp, prefix: &str) -> String {
    let name = common::username(prefix);
    let prefix = prefix.to_string();
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE name LIKE $1")
            .bind(format!("{prefix}-%"))
            .execute(&pool)
            .await;
    });
    app.create_and_login_admin(&name, PASSWORD, &format!("{name}@test.local"))
        .await
}

/// Insert a dead-lettered row directly; returns its id. Deleted when the
/// test ends.
async fn insert_dead(app: &TestApp, plugin: &str, reason: &str) -> i64 {
    let now = chrono::Utc::now().timestamp();
    let row = sqlx::query(
        r#"
        INSERT INTO plugin_queue
            (plugin_name, queue_name, payload, created_at, status, attempts,
             max_attempts, dead_reason, dead_at, last_error)
        VALUES ($1, 'q', '{"x":1}'::jsonb, $2, 'dead', 3, 3, $3, $2, $3)
        RETURNING id
        "#,
    )
    .bind(plugin)
    .bind(now)
    .bind(reason)
    .fetch_one(&app.db)
    .await
    .unwrap();
    let id: i64 = row.get(0);
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM plugin_queue WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

async fn status_of(app: &TestApp, id: i64) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT status FROM plugin_queue WHERE id = $1")
        .bind(id)
        .fetch_optional(&app.db)
        .await
        .unwrap()
}

#[test]
fn the_page_lists_a_seeded_dead_job_only_for_administer_site() {
    run_test(async {
        let app = shared_app().await;
        let tag = tag();
        let id = insert_dead(app, &tag, "boom, repeatedly").await;

        // A signed-in account without `administer site` is refused.
        let plain_name = common::username("djplain");
        let plain = app
            .create_and_login_user(&plain_name, PASSWORD, &format!("{plain_name}@test.local"))
            .await;
        let pool = app.db.clone();
        defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM users WHERE name = $1")
                .bind(&plain_name)
                .execute(&pool)
                .await;
        });
        let (status, _) = get(app, &plain, "/admin/reports/dead-jobs").await;
        assert_ne!(status, StatusCode::OK, "no permission, no page");

        // An administrator sees the seeded job, filtered to its plugin.
        let cookies = admin(app, "djlist").await;
        let (status, html) = get(
            app,
            &cookies,
            &format!("/admin/reports/dead-jobs?plugin={tag}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(&tag), "the job's plugin is shown");
        assert!(html.contains("boom, repeatedly"), "the reason is shown");
        assert!(html.contains(&id.to_string()), "the job id is shown");
    });
}

#[test]
fn retry_moves_a_dead_job_back_to_ready() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "djretry").await;
        let tag = tag();
        let id = insert_dead(app, &tag, "will be retried").await;

        let (status, page) = get(
            app,
            &cookies,
            &format!("/admin/reports/dead-jobs?plugin={tag}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains(&format!(r#"action="/admin/reports/dead-jobs/{id}/retry""#)));

        let (status, location, _) = post(
            app,
            &cookies,
            &format!("/admin/reports/dead-jobs/{id}/retry"),
            format!("_token={}", encode(&token(&page))),
        )
        .await;
        assert!(status.is_redirection(), "{status}");
        assert_eq!(location, "/admin/reports/dead-jobs");
        assert_eq!(status_of(app, id).await.as_deref(), Some("ready"));

        let (attempts, dead_reason): (i32, Option<String>) =
            sqlx::query_as("SELECT attempts, dead_reason FROM plugin_queue WHERE id = $1")
                .bind(id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(attempts, 0);
        assert!(dead_reason.is_none());
    });
}

#[test]
fn discard_asks_first_through_confirm_then_deletes() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "djdiscard").await;
        let tag = tag();
        let id = insert_dead(app, &tag, "will be discarded").await;

        let (_, page) = get(
            app,
            &cookies,
            &format!("/admin/reports/dead-jobs?plugin={tag}"),
        )
        .await;
        assert!(
            page.contains(&format!(
                r#"action="&#x2F;admin&#x2F;reports&#x2F;dead-jobs&#x2F;{id}&#x2F;discard?confirm""#
            )),
            "the row's discard form asks first"
        );

        let question = format!("Discard job {id}? This cannot be undone.");
        let label = format!("Discard job {id}");
        let form = format!(
            "_token={}&_confirm={}&_confirm_label={}",
            encode(&token(&page)),
            encode(&question),
            encode(&label),
        );
        let (status, location, _) = post(
            app,
            &cookies,
            &format!("/admin/reports/dead-jobs/{id}/discard?confirm"),
            form,
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(location, "/admin/confirm");
        assert_eq!(
            status_of(app, id).await.as_deref(),
            Some("dead"),
            "nothing is discarded before confirming"
        );

        let (status, confirm) = get(app, &cookies, "/admin/confirm").await;
        assert_eq!(status, StatusCode::OK);
        assert!(confirm.contains(&question));
        assert!(confirm.contains(&format!(
            r#"action="&#x2F;admin&#x2F;reports&#x2F;dead-jobs&#x2F;{id}&#x2F;discard""#
        )));

        let (status, location, _) = post(
            app,
            &cookies,
            &format!("/admin/reports/dead-jobs/{id}/discard"),
            format!("_token={}", encode(&token(&confirm))),
        )
        .await;
        assert!(status.is_redirection(), "{status}");
        assert_eq!(location, "/admin/reports/dead-jobs");
        assert!(
            status_of(app, id).await.is_none(),
            "confirming discards the job"
        );
    });
}

#[test]
fn a_forged_discard_is_refused_not_asked_about() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "djforge").await;
        let tag = tag();
        let id = insert_dead(app, &tag, "should survive").await;

        let (status, location, _) = post(
            app,
            &cookies,
            &format!("/admin/reports/dead-jobs/{id}/discard?confirm"),
            "_token=forged&_confirm=Anything%20at%20all".to_string(),
        )
        .await;
        assert_ne!(location, "/admin/confirm", "{status}");
        assert_eq!(status_of(app, id).await.as_deref(), Some("dead"));
    });
}

#[test]
fn the_dashboard_links_the_dead_jobs_page() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin(app, "djdash").await;
        let tag = tag();
        let _id = insert_dead(app, &tag, "shown on the dashboard").await;

        let (status, html) = get(app, &cookies, "/admin").await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains(r#"href="/admin/reports/dead-jobs""#),
            "the Scheduled tasks panel links the dead jobs page"
        );
    });
}
