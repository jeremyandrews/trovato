#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The timers report (B278): `/admin/reports/timers`, its row actions, the
//! edit form and the throttles form.
//!
//! Every timer here belongs to an owner made for the test and the listing is
//! filtered to it, so other tests' timers never change what a test sees. The
//! shared app runs no scheduler, so a timer here never fires.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, user_holding};
use sqlx::PgPool;
use trovato_kernel::timer::{self, TimerRequest, TimerSettings};
use uuid::Uuid;

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
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

async fn post(app: &TestApp, cookies: &str, uri: &str, body: String) -> axum::response::Response {
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

/// The first form token on a page.
fn token(html: &str) -> String {
    let at = html.find(r#"name="_token" value=""#).expect("a form token") + 21;
    let end = html[at..].find('"').unwrap() + at;
    html[at..end].to_string()
}

/// A fresh token from the listing: each is good for one post.
async fn fresh_token(app: &TestApp, cookies: &str, owner: &str) -> String {
    let (status, html) = get(
        app,
        cookies,
        &format!("/admin/reports/timers?owner={owner}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    token(&html)
}

fn owner() -> String {
    format!("t_{}", &Uuid::new_v4().simple().to_string()[..12])
}

fn cleanup(pool: &PgPool, owner: &str) {
    let pool = pool.clone();
    let owner = owner.to_string();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM scheduled_timer WHERE owner = $1")
            .bind(owner)
            .execute(&pool)
            .await;
    });
}

/// Set a timer for `owner` an hour from now, and return its id.
async fn set(pool: &PgPool, owner: &str, name: &str, repeat_secs: Option<i64>) -> i64 {
    let request = TimerRequest {
        name: name.to_string(),
        run_at: chrono::Utc::now().timestamp() + 3600,
        repeat_secs,
        payload: serde_json::json!({}),
    };
    let settings = TimerSettings {
        max_per_plugin: 100_000,
        ..TimerSettings::default()
    };
    timer::set(pool, owner, &request, &settings)
        .await
        .expect("set")
        .id
}

async fn status_of(pool: &PgPool, id: i64) -> Option<String> {
    sqlx::query_scalar("SELECT status FROM scheduled_timer WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
        .unwrap()
}

/// The `timer.changed` events about `owner`'s timers, oldest first: actor,
/// action and details.
async fn changes(pool: &PgPool, owner: &str) -> Vec<(Option<Uuid>, String, serde_json::Value)> {
    sqlx::query_as(
        "SELECT actor_id, details->>'action', details FROM security_audit_log \
         WHERE kind = 'timer.changed' AND details->>'owner' = $1 ORDER BY created, id",
    )
    .bind(owner)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn admin(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let name = common::username(prefix);
    let cookies = app
        .create_and_login_admin(
            &name,
            "correct-horse-battery-staple",
            &format!("{name}@test.local"),
        )
        .await;
    (common::user_id_of(app, &name).await, cookies)
}

fn assert_refused(status: StatusCode, who: &str, what: &str) {
    assert!(
        status == StatusCode::FORBIDDEN
            || status == StatusCode::UNAUTHORIZED
            || status == StatusCode::NOT_FOUND
            || status.is_redirection(),
        "{what} must be refused to {who}, got {status}"
    );
}

/// Nobody without a timer permission opens the report; `view timers` opens
/// it without the row actions, and is refused every change; `administer
/// timers` gets both.
#[test]
fn the_report_takes_view_and_every_change_takes_administer() {
    run_test(async {
        let app = shared_app().await;
        let owner = owner();
        cleanup(&app.db, &owner);
        let id = set(&app.db, &owner, "perm:probe", None).await;

        let (_, plain) = user_holding(app, "b278plain", &["access administration pages"]).await;
        let (_, viewer) = user_holding(
            app,
            "b278view",
            &["access administration pages", "view timers"],
        )
        .await;
        let (_, manager) = user_holding(
            app,
            "b278admin",
            &["access administration pages", "administer timers"],
        )
        .await;
        let list = format!("/admin/reports/timers?owner={owner}");
        let edit = format!("/admin/reports/timers/{id}/edit");

        for (who, cookies) in [
            ("an anonymous visitor", ""),
            ("a user with neither", plain.as_str()),
        ] {
            for uri in [
                list.as_str(),
                edit.as_str(),
                "/admin/reports/timers/settings",
            ] {
                let (status, _) = get(app, cookies, uri).await;
                assert_refused(status, who, &format!("GET {uri}"));
            }
        }

        let (status, html) = get(app, &viewer, &list).await;
        assert_eq!(status, StatusCode::OK, "view timers opens the report");
        assert!(html.contains("perm:probe"));
        assert!(
            !html.contains(&format!("/admin/reports/timers/{id}/pause")),
            "no row actions"
        );
        assert!(
            !html.contains("/admin/reports/timers/settings"),
            "no throttles link"
        );
        for uri in [edit.as_str(), "/admin/reports/timers/settings"] {
            let (status, _) = get(app, &viewer, uri).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "view timers may not open {uri}"
            );
        }
        // Even with a good token, a viewer cannot change a timer.
        let tok = fresh_token(app, &manager, &owner).await;
        for action in ["pause", "resume", "run", "cancel"] {
            let response = post(
                app,
                &viewer,
                &format!("/admin/reports/timers/{id}/{action}"),
                format!("_token={tok}"),
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "view timers may not {action}"
            );
        }
        assert_eq!(status_of(&app.db, id).await.as_deref(), Some("scheduled"));

        let (status, html) = get(app, &manager, &list).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains(&format!("/admin/reports/timers/{id}/pause")),
            "row actions"
        );
        assert!(html.contains(&format!("/admin/reports/timers/{id}/edit")));
        let (status, _) = get(app, &manager, &edit).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = get(app, &manager, "/admin/reports/timers/settings").await;
        assert_eq!(status, StatusCode::OK);

        // A change without the page's token is refused and changes nothing.
        let response = post(
            app,
            &manager,
            &format!("/admin/reports/timers/{id}/pause"),
            "_token=forged".into(),
        )
        .await;
        assert!(!response.status().is_redirection(), "no token, no change");
        assert_eq!(status_of(&app.db, id).await.as_deref(), Some("scheduled"));
        assert!(changes(&app.db, &owner).await.is_empty(), "nothing logged");
    });
}

/// Pause, resume, run now, edit and cancel each do what they say and are
/// recorded in the security log with who did it and the before and after.
#[test]
fn each_row_action_changes_the_timer_and_is_in_the_security_log() {
    run_test(async {
        let app = shared_app().await;
        let owner = owner();
        cleanup(&app.db, &owner);
        let (admin_id, cookies) = admin(app, "b278act").await;
        let id = set(&app.db, &owner, "act:one", Some(3600)).await;
        let act = |action: &str| format!("/admin/reports/timers/{id}/{action}");

        let tok = fresh_token(app, &cookies, &owner).await;
        let response = post(app, &cookies, &act("pause"), format!("_token={tok}")).await;
        assert!(response.status().is_redirection(), "{}", response.status());
        assert_eq!(status_of(&app.db, id).await.as_deref(), Some("paused"));
        let (_, html) = get(
            app,
            &cookies,
            &format!("/admin/reports/timers?owner={owner}"),
        )
        .await;
        assert!(
            html.contains("Paused act:one."),
            "the flash says what happened"
        );
        assert!(
            html.contains(&act("resume")),
            "a paused timer offers Resume"
        );

        let tok = fresh_token(app, &cookies, &owner).await;
        post(app, &cookies, &act("resume"), format!("_token={tok}")).await;
        assert_eq!(status_of(&app.db, id).await.as_deref(), Some("scheduled"));

        let tok = fresh_token(app, &cookies, &owner).await;
        post(app, &cookies, &act("run"), format!("_token={tok}")).await;
        let due_in: f64 = sqlx::query_scalar(
            "SELECT EXTRACT(EPOCH FROM run_at - now())::float8 FROM scheduled_timer WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(due_in <= 1.0, "run now makes it due now, got {due_in} s");

        // Edit: a bad repeat is refused with the reason, the rest kept as typed.
        let (status, html) = get(app, &cookies, &act("edit")).await;
        assert_eq!(status, StatusCode::OK);
        let response = post(
            app,
            &cookies,
            &act("edit"),
            format!(
                "_token={}&run_at=2031-01-02T03%3A04%3A05&repeat_secs=5",
                token(&html)
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "re-rendered");
        let html = body_string(response).await;
        assert!(html.contains("must be at least 60 seconds"), "{html}");
        assert!(
            html.contains("2031-01-02T03:04:05"),
            "the typed time is kept"
        );

        let (_, html) = get(app, &cookies, &act("edit")).await;
        let response = post(
            app,
            &cookies,
            &act("edit"),
            format!(
                "_token={}&run_at=2031-01-02T03%3A04%3A05&repeat_secs=7200",
                token(&html)
            ),
        )
        .await;
        assert!(response.status().is_redirection(), "{}", response.status());
        let (run_at, repeat): (chrono::DateTime<chrono::Utc>, Option<i32>) =
            sqlx::query_as("SELECT run_at, repeat_secs FROM scheduled_timer WHERE id = $1")
                .bind(id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        // The admin has no time zone: the field is in UTC.
        assert_eq!(run_at.to_rfc3339(), "2031-01-02T03:04:05+00:00");
        assert_eq!(repeat, Some(7200));

        // Clearing the repeat makes it run once.
        let (_, html) = get(app, &cookies, &act("edit")).await;
        post(
            app,
            &cookies,
            &act("edit"),
            format!(
                "_token={}&run_at=2031-01-02T03%3A04%3A05&repeat_secs=",
                token(&html)
            ),
        )
        .await;
        let repeat: Option<i32> =
            sqlx::query_scalar("SELECT repeat_secs FROM scheduled_timer WHERE id = $1")
                .bind(id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(repeat, None);

        // Cancel asks first: through the confirmation page...
        let tok = fresh_token(app, &cookies, &owner).await;
        let response = post(
            app,
            &cookies,
            &format!("{}?confirm", act("cancel")),
            format!("_token={tok}&_confirm=Cancel%3F&_confirm_label=Cancel"),
        )
        .await;
        assert!(response.status().is_redirection());
        assert_eq!(
            status_of(&app.db, id).await.as_deref(),
            Some("scheduled"),
            "not before the answer"
        );
        // ...and the confirmed post cancels it.
        let tok = fresh_token(app, &cookies, &owner).await;
        let response = post(app, &cookies, &act("cancel"), format!("_token={tok}")).await;
        assert!(response.status().is_redirection());
        assert_eq!(status_of(&app.db, id).await, None, "cancelled");

        let log = changes(&app.db, &owner).await;
        let actions: Vec<&str> = log.iter().map(|(_, a, _)| a.as_str()).collect();
        assert_eq!(
            actions,
            ["pause", "resume", "run_now", "edit", "edit", "cancel"]
        );
        for (actor, action, details) in &log {
            assert_eq!(*actor, Some(admin_id), "{action}: who did it");
            assert_eq!(details["name"], "act:one");
            assert!(details.get("before").is_some(), "{action}: before");
        }
        assert_eq!(log[0].2["before"]["status"], "scheduled");
        assert_eq!(log[0].2["after"]["status"], "paused");
        assert_eq!(log[3].2["after"]["repeat_secs"], 7200);
        assert!(
            log[5].2["after"].is_null(),
            "a cancelled timer has no after"
        );

        // A timer that is gone is a 404, not an error.
        let (_, settings) = get(app, &cookies, "/admin/reports/timers/settings").await;
        let tok = token(&settings);
        let response = post(app, &cookies, &act("pause"), format!("_token={tok}")).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    });
}

/// The listing pages at 50, filters by status and name, and shows the
/// next run in the viewer's own time zone.
#[test]
fn the_listing_pages_filters_and_shows_the_viewers_time() {
    run_test(async {
        let app = shared_app().await;
        let owner = owner();
        cleanup(&app.db, &owner);
        let (admin_id, cookies) = admin(app, "b278list").await;
        for i in 0..55 {
            set(&app.db, &owner, &format!("page:{i:03}"), None).await;
        }
        let paused = set(&app.db, &owner, "other:paused", None).await;
        sqlx::query("UPDATE scheduled_timer SET status = 'paused' WHERE id = $1")
            .bind(paused)
            .execute(&app.db)
            .await
            .unwrap();

        let base = format!("/admin/reports/timers?owner={owner}&sort=name");
        let (_, first) = get(app, &cookies, &base).await;
        let (_, second) = get(app, &cookies, &format!("{base}&page=2")).await;
        let rows = |html: &str| html.matches("<th scope=\"row\"><code>").count();
        assert_eq!(rows(&first), 50);
        assert_eq!(rows(&second), 6);
        assert!(first.contains("other:paused"));
        assert!(second.contains("page:054"));
        assert!(!first.contains("page:054"));

        let (_, html) = get(
            app,
            &cookies,
            &format!("/admin/reports/timers?owner={owner}&status=paused"),
        )
        .await;
        assert_eq!(rows(&html), 1);
        assert!(html.contains("other:paused"));
        let (_, html) = get(
            app,
            &cookies,
            &format!("/admin/reports/timers?owner={owner}&q=page%3A01"),
        )
        .await;
        assert_eq!(rows(&html), 10, "page:010 to page:019");

        // Times are in the viewer's zone: Tokyo is nine hours ahead of UTC.
        let fixed = chrono::DateTime::parse_from_rfc3339("2032-06-01T12:00:00Z").unwrap();
        sqlx::query("UPDATE scheduled_timer SET run_at = $2 WHERE id = $1")
            .bind(paused)
            .bind(fixed)
            .execute(&app.db)
            .await
            .unwrap();
        let (_, html) = get(
            app,
            &cookies,
            &format!("/admin/reports/timers?owner={owner}&status=paused"),
        )
        .await;
        assert!(html.contains("2032-06-01 12:00:00"), "UTC without a zone");
        sqlx::query("UPDATE users SET timezone = 'Asia/Tokyo' WHERE id = $1")
            .bind(admin_id)
            .execute(&app.db)
            .await
            .unwrap();
        app.state.users().invalidate(admin_id);
        let (_, html) = get(
            app,
            &cookies,
            &format!("/admin/reports/timers?owner={owner}&status=paused"),
        )
        .await;
        assert!(html.contains("2032-06-01 21:00:00"), "Tokyo time");
        assert!(html.contains("Asia/Tokyo"), "the zone is named");
        assert!(
            html.contains("2032-06-01T12:00:00Z"),
            "machine-readable UTC"
        );
    });
}

/// The throttles form saves what it is given, refuses what is not a
/// positive number, and logs the change.
#[test]
fn the_throttles_form_saves_validates_and_logs() {
    run_test(async {
        let app = shared_app().await;
        let (admin_id, cookies) = admin(app, "b278set").await;
        let before = timer::saved_settings(&app.db).await.unwrap();
        {
            let pool = app.db.clone();
            common::defer_cleanup(move || async move {
                let _ = timer::save_settings(&pool, before).await;
            });
        }
        let since = chrono::Utc::now().timestamp() - 1;

        let (status, html) = get(app, &cookies, "/admin/reports/timers/settings").await;
        assert_eq!(status, StatusCode::OK);
        for field in [
            "max_concurrent",
            "max_per_plugin",
            "min_repeat_secs",
            "max_payload_bytes",
        ] {
            assert!(html.contains(&format!(r#"name="{field}""#)), "{field}");
        }
        let response = post(
            app,
            &cookies,
            "/admin/reports/timers/settings",
            format!(
                "_token={}&max_concurrent=0&max_per_plugin=x&min_repeat_secs=60&max_payload_bytes=4096",
                token(&html)
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "re-rendered");
        let html = body_string(response).await;
        assert_eq!(
            html.matches("Enter a whole number above 0.").count(),
            4,
            "summary and field, twice"
        );
        assert_eq!(
            timer::saved_settings(&app.db).await.unwrap(),
            before,
            "nothing saved"
        );

        let (_, html) = get(app, &cookies, "/admin/reports/timers/settings").await;
        let wanted = TimerSettings {
            max_concurrent: before.max_concurrent,
            max_per_plugin: before.max_per_plugin + 1,
            min_repeat_secs: before.min_repeat_secs,
            max_payload_bytes: before.max_payload_bytes,
        };
        let response = post(
            app,
            &cookies,
            "/admin/reports/timers/settings",
            format!(
                "_token={}&max_concurrent={}&max_per_plugin={}&min_repeat_secs={}&max_payload_bytes={}",
                token(&html),
                wanted.max_concurrent,
                wanted.max_per_plugin,
                wanted.min_repeat_secs,
                wanted.max_payload_bytes
            ),
        )
        .await;
        assert!(response.status().is_redirection(), "{}", response.status());
        assert_eq!(timer::saved_settings(&app.db).await.unwrap(), wanted);

        let (actor, details): (Option<Uuid>, serde_json::Value) = sqlx::query_as(
            "SELECT actor_id, details FROM security_audit_log \
             WHERE kind = 'timer.settings_changed' AND actor_id = $1 AND created >= $2",
        )
        .bind(admin_id)
        .bind(since)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(actor, Some(admin_id));
        assert_eq!(details["after"]["max_per_plugin"], wanted.max_per_plugin);
        assert_eq!(details["before"]["max_per_plugin"], before.max_per_plugin);

        // Without the token nothing is saved.
        let response = post(
            app,
            &cookies,
            "/admin/reports/timers/settings",
            "_token=forged&max_concurrent=9&max_per_plugin=9&min_repeat_secs=9&max_payload_bytes=9"
                .into(),
        )
        .await;
        assert!(!response.status().is_redirection());
        assert_eq!(timer::saved_settings(&app.db).await.unwrap(), wanted);
    });
}

/// The cron report and the timers report point at each other, each with
/// its one-line note.
#[test]
fn the_cron_report_and_the_timers_report_link_each_other() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = admin(app, "b278link").await;
        let (status, cron) = get(app, &cookies, "/admin/reports/cron").await;
        assert_eq!(status, StatusCode::OK);
        assert!(cron.contains(r#"<a href="/admin/reports/timers">Timers</a>"#));
        assert!(cron.contains("Work due at a known second"));
        let (status, timers) = get(app, &cookies, "/admin/reports/timers").await;
        assert_eq!(status, StatusCode::OK);
        assert!(timers.contains(r#"<a href="/admin/reports/cron">Cron</a>"#));
        assert!(timers.contains("Routine sweeps belong to cron."));
        let (_, reports) = get(app, &cookies, "/admin/reports/overview").await;
        assert!(
            reports.contains("/admin/reports/timers"),
            "listed under Reports"
        );
    });
}

/// The throttles form will not raise the payload limit past what a plugin
/// can read back.
#[test]
fn the_throttles_form_refuses_a_payload_limit_past_the_ceiling() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = admin(app, "b278cap").await;
        let before = timer::saved_settings(&app.db).await.unwrap();
        let (_, html) = get(app, &cookies, "/admin/reports/timers/settings").await;
        let response = post(
            app,
            &cookies,
            "/admin/reports/timers/settings",
            format!(
                "_token={}&max_concurrent={}&max_per_plugin={}&min_repeat_secs={}&max_payload_bytes=300000",
                token(&html),
                before.max_concurrent,
                before.max_per_plugin,
                before.min_repeat_secs
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "re-rendered");
        let html = body_string(response).await;
        assert!(
            html.contains("The largest payload can be at most 65536 bytes."),
            "{html}"
        );
        assert_eq!(
            timer::saved_settings(&app.db).await.unwrap(),
            before,
            "nothing saved"
        );
    });
}
