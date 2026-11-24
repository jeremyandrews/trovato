#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Who sees a request's counts in `Server-Timing` (BG14).
//!
//! Under the default `SERVER_TIMING=admin`: an anonymous visitor and a signed-in
//! member get exactly `total;dur=<ms>`, which is all anyone got before; an
//! administrator also gets the database, cache, plugin, render and outgoing
//! call breakdown. Under `SERVER_TIMING=off` the administrator gets `total`
//! only too. Whatever the header shows, the request's profile is in the
//! response, with real counts.
//!
//! Requires Postgres + Redis; runs in CI.

mod common;

use axum::body::Body;
use axum::http::Request;
use common::{TestApp, defer_cleanup, run_test, username};
use tracing_subscriber::layer::SubscriberExt;
use trovato_kernel::middleware::ServerTimingDetail;
use trovato_kernel::telemetry::RequestProfileSnapshot;

const PASSWORD: &str = "server-timing-test-pw";

/// The header and the profile of `GET /` with `cookies`.
async fn front(app: &TestApp, cookies: &str) -> (String, RequestProfileSnapshot) {
    let mut request = Request::get("/");
    if !cookies.is_empty() {
        request = request.header("cookie", cookies);
    }
    let response = app.request(request.body(Body::empty()).unwrap()).await;
    assert!(response.status().is_success(), "{}", response.status());
    let header = response
        .headers()
        .get("server-timing")
        .expect("every response has Server-Timing")
        .to_str()
        .unwrap()
        .to_string();
    let snapshot = response
        .extensions()
        .get::<RequestProfileSnapshot>()
        .cloned()
        .expect("the profile is in the response");
    (header, snapshot)
}

fn is_total_only(header: &str) -> bool {
    header
        .strip_prefix("total;dur=")
        .is_some_and(|ms| !ms.is_empty() && ms.bytes().all(|b| b.is_ascii_digit()))
}

/// Delete `name` when the test ends.
fn forget(app: &TestApp, name: &str) {
    let (pool, name) = (app.db.clone(), name.to_string());
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE name = $1")
            .bind(&name)
            .execute(&pool)
            .await;
    });
}

#[test]
fn only_an_administrator_sees_the_counts() {
    // The query listener, so the profile counts statements as in production.
    let subscriber = tracing_subscriber::registry()
        .with(trovato_kernel::telemetry::query_listener::query_listener());
    tracing::subscriber::set_global_default(subscriber).expect("one subscriber per binary");

    run_test(async {
        let app =
            TestApp::with_config(|c| c.runtime.server_timing = ServerTimingDetail::Admin).await;

        let (header, profile) = front(&app, "").await;
        assert!(is_total_only(&header), "anonymous got {header:?}");
        assert!(profile.queries > 0, "{profile:?}");

        let member = username("st_member");
        app.create_test_user(&member, PASSWORD, &format!("{member}@example.com"))
            .await;
        forget(&app, &member);
        let member_cookies = app.login(&member, PASSWORD).await;
        let (header, profile) = front(&app, &member_cookies).await;
        assert!(is_total_only(&header), "a member got {header:?}");
        assert!(profile.queries > 0, "{profile:?}");

        let admin = username("st_admin");
        app.create_test_admin(&admin, PASSWORD, &format!("{admin}@example.com"))
            .await;
        forget(&app, &admin);
        let admin_cookies = app.login(&admin, PASSWORD).await;
        let (header, profile) = front(&app, &admin_cookies).await;
        assert!(
            header.starts_with("total;dur=")
                && header.contains(", db;dur=")
                && header.contains(&format!("desc=\"{} queries\"", profile.queries))
                && header.contains(", cache;desc=")
                && header.contains(", plugin;dur=")
                && header.contains(", render;dur="),
            "an administrator got {header:?} for {profile:?}"
        );

        // A blocked administrator is nobody.
        sqlx::query("UPDATE users SET status = 0 WHERE name = $1")
            .bind(&admin)
            .execute(&app.db)
            .await
            .unwrap();
        app.state.users().clear_local();
        let (header, _) = front(&app, &admin_cookies).await;
        assert!(
            is_total_only(&header),
            "a blocked administrator got {header:?}"
        );
    });
}

#[test]
fn off_shows_the_counts_to_no_one() {
    run_test(async {
        let app = TestApp::with_config(|c| c.runtime.server_timing = ServerTimingDetail::Off).await;
        let admin = username("st_off_admin");
        app.create_test_admin(&admin, PASSWORD, &format!("{admin}@example.com"))
            .await;
        forget(&app, &admin);
        let cookies = app.login(&admin, PASSWORD).await;
        let (header, _) = front(&app, &cookies).await;
        assert!(
            is_total_only(&header),
            "SERVER_TIMING=off still showed {header:?}"
        );
    });
}
