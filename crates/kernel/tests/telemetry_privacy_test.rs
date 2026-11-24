#![allow(clippy::unwrap_used, clippy::expect_used)]
//! No secret and no personal data in any telemetry (BG14).
//!
//! Seeds a password, an email, a password reset token, an API token and a
//! search term, makes requests that carry each of them (in a body, a query
//! string, a path and a header), and fails if any of them appears in:
//!
//! - a log line, under the production `RUST_LOG` default;
//! - any Gander span or event, captured at `TRACE`, fields and all;
//! - a `Server-Timing` header, with the detail shown to everyone;
//! - `/metrics`.
//!
//! The signed-in user's id is checked against the spans, the header and
//! `/metrics` only: the existing login audit line records it for the site
//! log, as it should.
//!
//! Every request is logged as slow (a zero threshold), so the slow request
//! line with its breakdown is in the captured output too. The test also checks
//! that the capture saw what it should (spans, slow lines, real query counts),
//! so it cannot pass by capturing nothing.
//!
//! Requires Postgres + Redis; runs in CI.

mod common;

use std::sync::{Arc, Mutex};

use axum::body::{Body, to_bytes};
use axum::http::Request;
use common::{TestApp, run_test};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{EnvFilter, Targets};
use tracing_subscriber::layer::{Context, SubscriberExt};
use trovato_kernel::telemetry::RequestProfileSnapshot;

/// Everything captured, as text.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<String>>);

impl Captured {
    fn push(&self, s: &str) {
        let mut buf = self.0.lock().unwrap();
        buf.push_str(s);
        buf.push('\n');
    }
    fn text(&self) -> String {
        self.0.lock().unwrap().clone()
    }
}

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap()
            .push_str(&String::from_utf8_lossy(buf));
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Every field of a span or event, `name=value`.
struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!(" {}={value:?}", field.name()));
    }
}

/// Writes every span it is shown (on creation and on each `record`) and every
/// event into the capture.
struct CaptureAll(Captured);

impl<S: Subscriber> Layer<S> for CaptureAll {
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        let mut f = Fields(format!("SPAN {}", attrs.metadata().name()));
        attrs.record(&mut f);
        self.0.push(&f.0);
    }
    fn on_record(&self, _id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
        let mut f = Fields("RECORD".to_string());
        values.record(&mut f);
        self.0.push(&f.0);
    }
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut f = Fields(format!("EVENT {}", event.metadata().target()));
        event.record(&mut f);
        self.0.push(&f.0);
    }
}

fn csrf_from(html: &str) -> String {
    let pos = html.find("csrf-token").expect("csrf-token meta tag");
    let start = html[pos..].find("content=\"").map(|p| pos + p + 9).unwrap();
    let end = html[start..].find('"').map(|p| start + p).unwrap();
    html[start..end].to_string()
}

async fn send(
    app: &TestApp,
    request: Request<Body>,
    timings: &mut Vec<String>,
) -> (u16, String, Option<RequestProfileSnapshot>) {
    let response = app.request(request).await;
    let status = response.status().as_u16();
    if let Some(v) = response.headers().get("server-timing") {
        timings.push(v.to_str().unwrap().to_string());
    }
    let snapshot = response
        .extensions()
        .get::<RequestProfileSnapshot>()
        .cloned();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        String::from_utf8_lossy(&body).into_owned(),
        snapshot,
    )
}

#[test]
fn no_secret_reaches_any_telemetry() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .with_filter(EnvFilter::new("info,tower_http=debug,sqlx=warn")),
        )
        .with(
            CaptureAll(captured.clone())
                .with_filter(Targets::new().with_target("gander", tracing::Level::TRACE)),
        )
        .with(trovato_kernel::telemetry::query_listener::query_listener());
    tracing::subscriber::set_global_default(subscriber).expect("one subscriber per test binary");

    run_test(async move {
        let app = TestApp::with_config(|c| {
            c.runtime.slow_request_threshold_ms = 0;
            c.runtime.server_timing = trovato_kernel::middleware::ServerTimingDetail::All;
            c.runtime.profile_fingerprints = true;
        })
        .await;

        let tag = uuid::Uuid::now_v7().simple().to_string();
        let tag = &tag[tag.len() - 12..];
        let username = format!("bg14priv_{tag}");
        let email = format!("secret.mail.{tag}@example.com");
        let password = format!("Pw-secret-{tag}-x9!");
        let reset_token = format!("resettoken{tag}secret");
        let api_token = format!("trv_apitoken{tag}secret");
        let search_term = format!("searchterm{tag}");

        app.create_test_user(&username, &password, &email).await;
        {
            let (pool, name) = (app.db.clone(), username.clone());
            common::defer_cleanup(move || async move {
                let _ = sqlx::query("DELETE FROM users WHERE name = $1")
                    .bind(&name)
                    .execute(&pool)
                    .await;
            });
        }
        let user_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(&username)
            .fetch_one(&app.db)
            .await
            .unwrap();
        let user_id = user_id.to_string();
        let cookies = app.login(&username, &password).await;

        let mut timings = Vec::new();
        send(
            &app,
            Request::get("/")
                .header("cookie", &cookies)
                .body(Body::empty())
                .unwrap(),
            &mut timings,
        )
        .await;
        let (_, _, search) = send(
            &app,
            Request::get(format!(
                "/search?q={search_term}+{}",
                email.replace('@', "%40")
            ))
            .header("cookie", &cookies)
            .body(Body::empty())
            .unwrap(),
            &mut timings,
        )
        .await;
        send(
            &app,
            Request::get(format!("/user/password-reset/{reset_token}"))
                .body(Body::empty())
                .unwrap(),
            &mut timings,
        )
        .await;
        // The reset request reaches its handler: an anonymous session, its
        // CSRF token, and the email in the form body.
        let form = app
            .request(
                Request::get("/user/password-reset")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(form.status(), 200);
        let reset_cookies = common::extract_cookies(&form);
        let form_html =
            String::from_utf8_lossy(&to_bytes(form.into_body(), usize::MAX).await.unwrap())
                .into_owned();
        let csrf = csrf_from(&form_html);
        let (status, _, _) = send(
            &app,
            Request::post("/user/password-reset")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", &reset_cookies)
                .body(Body::from(format!(
                    "_token={}&email={}",
                    urlencoding::encode(&csrf),
                    email.replace('@', "%40")
                )))
                .unwrap(),
            &mut timings,
        )
        .await;
        assert_eq!(status, 200, "the reset request did not reach its handler");
        send(
            &app,
            Request::get("/api/v1/items")
                .header("authorization", format!("Bearer {api_token}"))
                .body(Body::empty())
                .unwrap(),
            &mut timings,
        )
        .await;
        let (metrics_status, metrics_body, _) = send(
            &app,
            Request::get("/metrics").body(Body::empty()).unwrap(),
            &mut timings,
        )
        .await;
        assert_eq!(metrics_status, 200);

        let output = captured.text();

        // The capture is not vacuous: it saw the request spans, the slow
        // request lines with their breakdown, and real counts.
        assert!(
            output.contains("SPAN http.request"),
            "no request span captured"
        );
        assert!(
            output.contains("SPAN template.render"),
            "no render span captured"
        );
        assert!(output.contains("SPAN cache"), "no cache span captured");
        assert!(
            output.contains("slow request"),
            "no slow request line captured"
        );
        assert!(
            output.lines().any(|l| l.contains("slow request")
                && l.contains(" queries=")
                && l.contains(" db_ms=")
                && l.contains(" route=")),
            "the slow line carries no breakdown"
        );
        // The tower_http request span is printed, so the URI check has
        // something to look at.
        assert!(
            output.contains("request{method="),
            "no request span in the log"
        );
        // The metrics this part feeds are there.
        let db_count: u64 = metrics_body
            .lines()
            .find_map(|l| l.strip_prefix("db_query_duration_seconds_count "))
            .and_then(|v| v.trim().parse().ok())
            .expect("a db_query_duration_seconds_count line");
        assert!(db_count > 0, "/metrics observed no query durations");
        assert!(
            metrics_body
                .lines()
                .any(|l| l.starts_with("http_request_db_queries_count")
                    && l.contains("route=\"/search\"")),
            "/metrics has no queries per request for /search:\n{}",
            metrics_body
                .lines()
                .filter(|l| l.contains("db_queries"))
                .take(5)
                .collect::<Vec<_>>()
                .join("\n")
        );
        let search = search.expect("the profile snapshot is in the response");
        assert!(
            search.queries > 0,
            "the search request counted no queries: {search:?}"
        );
        assert!(!search.fingerprints.is_empty());
        assert!(timings.iter().any(|t| t.contains("queries")), "{timings:?}");

        // The user id is in the existing login audit line ("user logged in",
        // for the site log), so it is checked against what this telemetry
        // adds: the Gander spans and events, Server-Timing and /metrics.
        let added = output
            .lines()
            .filter(|l| {
                l.starts_with("SPAN ") || l.starts_with("RECORD") || l.starts_with("EVENT ")
            })
            .chain(timings.iter().map(String::as_str))
            .chain(metrics_body.lines())
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(line) = added.lines().find(|l| l.contains(user_id.as_str())) {
            panic!("the user id reached telemetry:\n{line}");
        }
        let everything = format!("{output}\n{}\n{metrics_body}", timings.join("\n"));
        for (what, secret) in [
            ("password", password.as_str()),
            ("email", email.as_str()),
            ("email (encoded)", &email.replace('@', "%40")),
            ("email local part", &format!("secret.mail.{tag}")),
            ("reset token", reset_token.as_str()),
            ("API token", api_token.as_str()),
            ("search term", search_term.as_str()),
        ] {
            if let Some(at) = everything.find(secret) {
                let start = everything[..at].rfind('\n').map_or(0, |p| p + 1);
                let end = everything[at..]
                    .find('\n')
                    .map_or(everything.len(), |p| at + p);
                panic!("the {what} reached telemetry:\n{}", &everything[start..end]);
            }
        }
    });
}
