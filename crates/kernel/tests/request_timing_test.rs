#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `track_request_timing` is applied, so every response reports its duration.
//!
//! The middleware existed, was exported, had unit tests for its threshold
//! arithmetic, and was attached to no router: `QUERY_SLOW_THRESHOLD_MS` did
//! nothing and no response ever carried `Server-Timing`. Dead middleware that
//! reads a documented setting is worse than no middleware, because the setting
//! is a promise.
//!
//! The unit tests in the module cover the slow/very-slow boundaries. What could
//! not be tested there is the thing that was actually wrong: whether the layer
//! is in the stack at all.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, project_root, run_test, shared_app};

/// Its own client address, so this file does not spend the shared rate-limit
/// bucket that requests without `X-Forwarded-For` fall into.
const CLIENT_IP: &str = "10.74.0.1";

async fn timing_header(app: &TestApp, path: &str) -> (StatusCode, Option<String>) {
    let response = app
        .request(
            Request::get(path)
                .header("x-forwarded-for", CLIENT_IP)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    let header = response
        .headers()
        .get("server-timing")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    (response.status(), header)
}

#[test]
fn every_response_carries_a_server_timing_duration() {
    run_test(async {
        let app = shared_app().await;

        // A page, a static asset and a 404: the layer is outside routing, so the
        // header does not depend on which route answered, or whether one did.
        for path in ["/", "/static/js/trovato.js", "/no-such-path-at-all"] {
            let (status, header) = timing_header(app, path).await;
            let header =
                header.unwrap_or_else(|| panic!("{path} answered {status} with no Server-Timing"));

            let duration = header
                .strip_prefix("total;dur=")
                .unwrap_or_else(|| panic!("{path}: unexpected Server-Timing format: {header}"));
            duration
                .parse::<u128>()
                .unwrap_or_else(|e| panic!("{path}: duration {duration:?} is not a number: {e}"));
        }
    });
}

/// The production router applies it too.
///
/// The test above rides the test harness's router, which mirrors `main.rs` but
/// is not it — and "attached to no router" was exactly the defect. So the
/// server's own layer stack is asserted directly.
#[test]
fn the_production_router_applies_the_timing_layer() {
    let main = std::fs::read_to_string(project_root().join("crates/kernel/src/main.rs"))
        .expect("read main.rs");
    assert!(
        main.contains("middleware::track_request_timing"),
        "main.rs must apply track_request_timing to the router"
    );
}
