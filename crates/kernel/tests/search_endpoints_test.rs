#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The AI search endpoints the search page actually calls.
//!
//! `trovato_scolta` declared `/api/scolta/v1/*` with page-style routes and no
//! `tap_api`, so none of them registered: the paths 404ed and its three worker
//! functions were dead code. The kernel serves the real endpoints at
//! `/api/v1/search/*`, and `static/js/scolta.js` defaulted to the plugin's dead
//! namespace — working only because `templates/search.html` overrode all three.
//!
//! With the plugin retired, the client defaults are the contract, so they are
//! pinned here against the routes the kernel registers.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};

/// The three endpoints, as `scolta.js` defaults to them.
const ENDPOINTS: [&str; 3] = [
    "/api/v1/search/expand",
    "/api/v1/search/summarize",
    "/api/v1/search/followup",
];

async fn text_of(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The client's default endpoints are routes the kernel serves.
///
/// A POST with no CSRF token is refused rather than 404: refusal proves the route
/// exists, which is the thing the dead plugin's namespace could not do. Asserting
/// a successful AI call would need a provider.
#[test]
fn the_client_default_endpoints_are_registered_routes() {
    run_test(async {
        let app = shared_app().await;

        for path in ENDPOINTS {
            let response = app
                .request(
                    Request::post(path)
                        .header("content-type", "application/json")
                        .body(Body::from("{\"query\":\"anything\"}"))
                        .unwrap(),
                )
                .await;

            assert_ne!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{path} must be a registered route, not a 404"
            );
        }
    });
}

/// The retired plugin's namespace is gone, so nothing can quietly start relying
/// on it again.
#[test]
fn the_retired_plugin_namespace_is_not_served() {
    run_test(async {
        let app = shared_app().await;

        for path in [
            "/api/scolta/v1/expand-query",
            "/api/scolta/v1/summarize",
            "/api/scolta/v1/follow-up",
            "/api/scolta/v1/health",
        ] {
            let response = app
                .request(
                    Request::post(path)
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await;

            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{path} belonged to a plugin that never served it"
            );
        }
    });
}

/// The search page relies on the client's defaults rather than restating them, so
/// a drift between the two cannot hide behind an override again.
#[test]
fn the_search_page_does_not_override_the_endpoints() {
    run_test(async {
        let app = shared_app().await;

        let response = app
            .request(Request::get("/search").body(Body::empty()).unwrap())
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = text_of(response).await;

        assert!(
            html.contains("scolta-config"),
            "the search page must still carry its config block"
        );
        assert!(
            !html.contains("\"endpoints\""),
            "the page must not restate the endpoints, page was:\n{html}"
        );
    });
}

/// And the shipped client file defaults to the kernel's paths. Reading the asset
/// keeps the assertion honest: this is the value a site gets with no config.
#[test]
fn the_client_file_defaults_to_the_kernel_endpoints() {
    let js = std::fs::read_to_string(common::project_root().join("static/js/scolta.js"))
        .expect("read scolta.js");

    for path in ENDPOINTS {
        assert!(
            js.contains(path),
            "scolta.js must default to {path}, so a site with no endpoint config works"
        );
    }
    assert!(
        !js.contains("/api/scolta/v1/"),
        "the dead namespace must not survive as a default"
    );
}

/// A test helper referencing `TestApp` keeps the unused-import lint quiet in
/// builds where only the file-reading test runs.
#[allow(dead_code)]
fn _uses_test_app(_: &TestApp) {}
