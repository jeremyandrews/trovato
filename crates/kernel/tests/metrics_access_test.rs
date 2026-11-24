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
use axum::http::{Method, Request, StatusCode};
use common::{TestApp, run_test};

const TOKEN: &str = "metrics-access-test-token-0123456789abcdef";
const HEALTH_TOKEN: &str = "health-probe-token-not-for-metrics-0123456789";

async fn scrape(app: &TestApp, headers: &[(&str, &str)]) -> (StatusCode, String) {
    send(app, Method::GET, headers).await
}

async fn send(app: &TestApp, method: Method, headers: &[(&str, &str)]) -> (StatusCode, String) {
    // Every answer, given or refused, is marked never to be stored.
    let mut request = Request::builder().method(method).uri("/metrics");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app.request(request.body(Body::empty()).unwrap()).await;
    let status = response.status();
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store"),
        "{status}"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[test]
fn metrics_answer_an_operator_and_nobody_else() {
    run_test(async {
        let app = TestApp::with_config(|c| {
            c.runtime.metrics_token = Some(TOKEN.to_string());
            c.health_token = Some(HEALTH_TOKEN.to_string());
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
        // The token on a same-host scrape: the API token and JWT middleware
        // leave an operator's bearer alone rather than refusing it as theirs.
        let (status, _) = scrape(&app, &[("authorization", bearer.as_str())]).await;
        assert_eq!(status, StatusCode::OK, "the token from the same host");

        // HEAD follows the same rule as GET.
        let (status, _) = send(&app, Method::HEAD, &[]).await;
        assert_eq!(status, StatusCode::OK, "HEAD from the same host");
        let (status, _) = send(&app, Method::HEAD, &[("x-forwarded-for", "203.0.113.9")]).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "HEAD through a proxy");

        // Each token opens its own door only: the probe token is not the
        // metrics token, and the metrics token is not the probe token.
        let probe = format!("Bearer {HEALTH_TOKEN}");
        let (status, _) = scrape(
            &app,
            &[
                ("x-forwarded-for", "203.0.113.9"),
                ("authorization", probe.as_str()),
            ],
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "the health token read /metrics"
        );
        let health = |token: &str| {
            Request::get("/health/ready")
                .header("x-forwarded-for", "203.0.113.9")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap()
        };
        let detailed = |body: &str| body.contains("\"postgres\"");
        let as_probe = app.request(health(HEALTH_TOKEN)).await;
        assert!(
            matches!(as_probe.status().as_u16(), 200 | 503),
            "{}",
            as_probe.status()
        );
        let as_probe = to_bytes(as_probe.into_body(), usize::MAX).await.unwrap();
        assert!(
            detailed(&String::from_utf8_lossy(&as_probe)),
            "the health token does not open the health details: {as_probe:?}"
        );
        let as_metrics = app.request(health(TOKEN)).await;
        assert!(
            matches!(as_metrics.status().as_u16(), 200 | 503),
            "{}",
            as_metrics.status()
        );
        let as_metrics = to_bytes(as_metrics.into_body(), usize::MAX).await.unwrap();
        assert!(
            !detailed(&String::from_utf8_lossy(&as_metrics)),
            "the metrics token opened the health details"
        );

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

#[test]
fn without_a_metrics_token_only_the_same_host_reads_metrics() {
    run_test(async {
        let app = TestApp::with_config(|c| c.runtime.metrics_token = None).await;
        let (status, _) = scrape(&app, &[]).await;
        assert_eq!(status, StatusCode::OK);
        // No token is configured, so no bearer opens it from elsewhere.
        let (status, _) = scrape(
            &app,
            &[
                ("x-forwarded-for", "203.0.113.9"),
                ("authorization", "Bearer "),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    });
}
