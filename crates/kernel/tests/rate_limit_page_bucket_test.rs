#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Page views have their own rate-limit bucket (B107).
//!
//! Every `GET` outside the specific categories fell through to `api`, 100 a
//! minute per address, so a reader browsing from a shared address (an office,
//! a school, a carrier NAT) was served a 429 for reading pages. A `GET` or
//! `HEAD` outside `/api/` is now a `page` request, 600 a minute by default and
//! configured like every other bucket (`TROVATO_RATE_LIMIT_PAGE`,
//! `rate_limit.page`). `/api/*` keeps the `api` bucket.
//!
//! Against [`TestApp::with_rate_limits`], which has the rate-limit layers where
//! `main.rs` has them. Each half comes from an address of its own.
//!
//! Requires Postgres and Redis; runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test};
use trovato_kernel::middleware::categorize_path;
use uuid::Uuid;

fn fresh_ip() -> String {
    common::test_ip_for(&Uuid::now_v7().to_string())
}

/// How many of `count` anonymous GETs of `uri`, from one new address, were
/// answered 429.
async fn refused_of(app: &TestApp, uri: &str, count: u32) -> u32 {
    let ip = fresh_ip();
    let mut refused = 0;
    for _ in 0..count {
        let response = app
            .request(
                Request::get(uri)
                    .header("x-forwarded-for", &ip)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            refused += 1;
        }
    }
    refused
}

#[test]
fn a_reader_browses_past_the_api_limit_and_the_api_does_not() {
    run_test(async {
        assert_eq!(categorize_path("/", "GET"), "page");
        assert_eq!(categorize_path("/api/v1/items", "GET"), "api");
        assert_eq!(categorize_path("/user/login", "POST"), "login");
        assert_eq!(categorize_path("/user/register", "POST"), "register");

        let app = TestApp::with_rate_limits(|_| {}).await;
        let (page_limit, _) = app.state.rate_limiter().config().bucket("page").unwrap();
        let (api_limit, _) = app.state.rate_limiter().config().bucket("api").unwrap();
        assert!(
            api_limit < 150 && page_limit > 150,
            "the test assumes the defaults: page {page_limit}, api {api_limit}"
        );

        // **The finding.** 150 page views in a minute, one address, no 429.
        let refused = refused_of(&app, "/", 150).await;
        assert_eq!(refused, 0, "{refused} of 150 page views were refused");

        // 150 API reads in a minute from one address still hit the API limit.
        let refused = refused_of(&app, "/api/v1/items", 150).await;
        assert!(
            refused >= 150 - api_limit,
            "the API limit of {api_limit} must refuse the rest of 150, refused {refused}"
        );
    });
}
