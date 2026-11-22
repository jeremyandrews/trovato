#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The cron report and the site log screen (B107).
//!
//! `/admin/reports/cron` shows the last run and runs cron now from a button;
//! `/admin/reports/log` pages through `system_log` by level and type. Both take
//! `administer site`: an anonymous visitor and a signed-in user without it are
//! refused, which is tested here for every new route, GET and POST.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, user_holding};
use trovato_kernel::system_log::{self, LogRecord};
use uuid::Uuid;

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).replace("&#x2F;", "/")
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn post_form(
    app: &TestApp,
    cookies: &str,
    uri: &str,
    body: String,
) -> axum::response::Response {
    app.request_with_cookies(
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
}

fn input_value(html: &str, name: &str) -> Option<String> {
    let needle = format!(r#"name="{name}""#);
    let at = html.find(&needle)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(tag[vat..vend].to_string())
}

/// The token and build id of the form whose `action` is `action`.
fn form_pairs(html: &str, action: &str) -> String {
    let at = html
        .find(&format!(r#"action="{action}""#))
        .unwrap_or_else(|| panic!("a form posting to {action}"));
    let scoped = &html[html[..at].rfind("<form").unwrap()..];
    let scoped = &scoped[..scoped.find("</form>").unwrap()];
    format!(
        "_token={}&_form_build_id={}",
        urlencoding::encode(&input_value(scoped, "_token").unwrap()),
        urlencoding::encode(&input_value(scoped, "_form_build_id").unwrap()),
    )
}

async fn admin_cookies(app: &TestApp, prefix: &str) -> String {
    let name = common::username(prefix);
    app.create_and_login_admin(
        &name,
        "correct-horse-battery-staple",
        &format!("{name}@test.local"),
    )
    .await
}

/// Refused: not a 200 and not a redirect into the report.
fn assert_refused(status: StatusCode, who: &str, what: &str) {
    assert!(
        status == StatusCode::FORBIDDEN
            || status == StatusCode::UNAUTHORIZED
            || status == StatusCode::NOT_FOUND
            || status.is_redirection(),
        "{what} must be refused to {who}, got {status}"
    );
    assert_ne!(status, StatusCode::OK, "{what} served {who}");
}

/// Every new report route refuses an anonymous visitor and a signed-in user
/// without `administer site`, GET and POST.
#[test]
fn the_reports_refuse_anonymous_and_plain_users() {
    run_test(async {
        let app = shared_app().await;
        let (_, plain) = user_holding(app, "b107rplain", &["access content"]).await;

        for (who, cookies) in [
            ("an anonymous visitor", ""),
            ("a plain user", plain.as_str()),
        ] {
            for uri in ["/admin/reports/cron", "/admin/reports/log"] {
                let (status, _) = get(app, cookies, uri).await;
                assert_refused(status, who, &format!("GET {uri}"));
            }
            let response = post_form(app, cookies, "/admin/reports/cron", "op=Run".into()).await;
            assert_refused(response.status(), who, "POST /admin/reports/cron");
        }

        // A plain user's refusal is a 403, not a login prompt: they are signed in.
        let (status, _) = get(app, &plain, "/admin/reports/log").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    });
}

/// **The finding.** An administrator runs cron from the report, and the
/// report then shows the run: when, the result, and the tasks.
#[test]
fn run_now_runs_cron_and_the_report_shows_the_run() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_cookies(app, "b107cron").await;

        let (status, html) = get(app, &cookies, "/admin/reports/cron").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Run cron now"), "the Run now button");

        // Without the form's token the run is refused.
        let response = post_form(app, &cookies, "/admin/reports/cron", "op=Run".into()).await;
        assert_ne!(response.status(), StatusCode::SEE_OTHER);
        assert!(!response.status().is_redirection(), "no token, no run");

        let body = form_pairs(&html, "/admin/reports/cron");
        let response = post_form(app, &cookies, "/admin/reports/cron", body).await;
        assert!(
            response.status().is_redirection(),
            "Run now redirects back, got {}",
            response.status()
        );
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "/admin/reports/cron"
        );

        let (status, html) = get(app, &cookies, "/admin/reports/cron").await;
        assert_eq!(status, StatusCode::OK);
        // Another test binary's cron may hold the lock; either way the report
        // says what happened.
        assert!(
            html.contains("Cron ran in") || html.contains("already running"),
            "the flash says what Run now did"
        );
        if html.contains("Cron ran in") {
            assert!(html.contains("completed"), "the result");
            assert!(html.contains("cleanup_temp_files"), "the tasks run");
            assert!(html.contains(" ms</td>"), "the duration");
        }
    });
}

/// The log screen lists entries and filters them by level and type.
#[test]
fn the_log_screen_lists_and_filters_entries() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_cookies(app, "b107log").await;

        let tag = Uuid::now_v7().simple().to_string();
        let now = chrono::Utc::now().timestamp();
        let record = |level: &'static str, log_type: &str, what: &str| LogRecord {
            created: now,
            level,
            log_type: log_type.to_string(),
            message: format!("{what} {tag}"),
            context: Default::default(),
        };
        let log_type = format!("b107test{}", &tag[..8]);
        system_log::insert(
            &app.db,
            &[
                record("error", &log_type, "disk on fire"),
                record("warn", &log_type, "disk getting warm"),
                record("info", "user", "someone signed in"),
            ],
        )
        .await
        .unwrap();
        let db = app.db.clone();
        let cleanup_tag = tag.clone();
        common::defer_cleanup(move || async move {
            sqlx::query("DELETE FROM system_log WHERE message LIKE $1")
                .bind(format!("%{cleanup_tag}"))
                .execute(&db)
                .await
                .ok();
        });

        let (status, html) = get(app, &cookies, "/admin/reports/log").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(&format!("disk on fire {tag}")));
        assert!(html.contains(&format!("disk getting warm {tag}")));

        let (status, html) = get(
            app,
            &cookies,
            &format!("/admin/reports/log?level=error&type={log_type}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(&format!("disk on fire {tag}")));
        assert!(
            !html.contains(&format!("disk getting warm {tag}")),
            "the level filter"
        );
        assert!(
            !html.contains(&format!("someone signed in {tag}")),
            "the type filter"
        );

        // An unknown level is ignored rather than an error.
        let (status, _) = get(app, &cookies, "/admin/reports/log?level=bogus&page=0").await;
        assert_eq!(status, StatusCode::OK);
    });
}

/// Retention by age removes old rows and leaves new ones.
#[test]
fn prune_removes_entries_older_than_the_retention() {
    run_test(async {
        let app = shared_app().await;
        let tag = Uuid::now_v7().simple().to_string();
        let now = chrono::Utc::now().timestamp();
        let at = |created: i64, what: &str| LogRecord {
            created,
            level: "warn",
            log_type: "b107prune".to_string(),
            message: format!("{what} {tag}"),
            context: Default::default(),
        };
        system_log::insert(
            &app.db,
            &[at(now - 40 * 86_400, "old"), at(now - 86_400, "recent")],
        )
        .await
        .unwrap();

        // A row cap far above anything the shared table holds, so only age
        // applies and no other test's rows are touched.
        system_log::prune(&app.db, 30, i64::MAX / 2).await.unwrap();

        let left: Vec<String> =
            sqlx::query_scalar("SELECT message FROM system_log WHERE message LIKE $1")
                .bind(format!("%{tag}"))
                .fetch_all(&app.db)
                .await
                .unwrap();
        assert_eq!(left, vec![format!("recent {tag}")]);

        sqlx::query("DELETE FROM system_log WHERE message LIKE $1")
            .bind(format!("%{tag}"))
            .execute(&app.db)
            .await
            .unwrap();
    });
}
