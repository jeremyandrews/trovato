#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `/metrics` answers an operator and nobody else (BG14).
//!
//! It used to answer anyone who could reach the server: route templates,
//! request rates, pool sizes, circuit breaker states. Now it answers a
//! loopback peer that is not a proxy relaying someone else, or a request with
//! `Authorization: Bearer <METRICS_TOKEN>`; anyone else gets a 404, as if the
//! route did not exist. The test app's peer is loopback (as a scraper on the
//! same host), so a forwarding header is what makes a request "someone else".
//!
//! Requires Postgres + Redis; runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test};

const TOKEN: &str = "metrics-access-test-token-0123456789abcdef";

async fn scrape(app: &TestApp, headers: &[(&str, &str)]) -> (StatusCode, String) {
    let mut request = Request::get("/metrics");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app.request(request.body(Body::empty()).unwrap()).await;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[test]
fn metrics_answer_an_operator_and_nobody_else() {
    run_test(async {
        let app = TestApp::with_config(|c| {
            c.runtime.metrics_token = Some(TOKEN.to_string());
        })
        .await;

        // A scraper on the same host.
        let (status, body) = scrape(&app, &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("# TYPE http_requests counter"), "{body}");

        // A visitor through a reverse proxy on the same host: the peer is
        // loopback, but the request is someone else's.
        for header in ["x-forwarded-for", "x-real-ip"] {
            let (status, body) = scrape(&app, &[(header, "203.0.113.9")]).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{header}");
            assert!(
                !body.contains("# TYPE http_requests counter"),
                "{header}: {body}"
            );
        }
        let (status, _) = scrape(&app, &[("forwarded", "for=203.0.113.9")]).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // The token holder, from anywhere.
        let bearer = format!("Bearer {TOKEN}");
        let (status, body) = scrape(
            &app,
            &[
                ("x-forwarded-for", "203.0.113.9"),
                ("authorization", bearer.as_str()),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("# TYPE http_requests counter"));

        // A wrong token, or the right one with a byte missing, earns nothing.
        let short = format!("Bearer {}", &TOKEN[1..]);
        for wrong in ["Bearer not-the-token", short.as_str(), TOKEN] {
            let (status, _) = scrape(
                &app,
                &[("x-forwarded-for", "203.0.113.9"), ("authorization", wrong)],
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{wrong}");
        }
    });
}
