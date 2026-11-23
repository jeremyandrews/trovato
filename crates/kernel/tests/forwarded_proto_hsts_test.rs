#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `X-Forwarded-Proto` decides `Strict-Transport-Security` only from a trusted
//! proxy.
//!
//! The security headers middleware sends HSTS when the request arrived over
//! HTTPS, and behind a TLS-terminating proxy the only way it can know that is
//! the proxy's `X-Forwarded-Proto`. It used to believe that header from any
//! socket peer, the one forwarding header still read without the
//! `TRUSTED_PROXIES` gate `X-Forwarded-For` goes through. These requests go
//! through the whole application, so they exercise the middleware as layered in
//! production, with the socket peer the harness supplies (127.0.0.1).
//!
//! Requires Postgres + Redis (the `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::Request;
use common::{TestApp, run_test, shared_app};

const HSTS: &str = "strict-transport-security";

fn health_over_https() -> Request<Body> {
    Request::get("/health")
        .header("x-forwarded-proto", "https")
        .body(Body::empty())
        .unwrap()
}

/// The harness peer is 127.0.0.1; with `TRUSTED_PROXIES` empty it is a direct,
/// untrusted client, and its claim that the request was HTTPS is not believed.
#[test]
fn forwarded_proto_from_an_untrusted_peer_gets_no_hsts() {
    run_test(async {
        let app = TestApp::with_config(|config| {
            config.trusted_proxies = trovato_kernel::client_addr::TrustedProxies::default();
        })
        .await;
        let response = app.request(health_over_https()).await;
        assert!(
            response.headers().get(HSTS).is_none(),
            "an untrusted peer's X-Forwarded-Proto must not earn HSTS, got {:?}",
            response.headers().get(HSTS)
        );
        // The rest of the security header set is still written.
        assert!(response.headers().get("x-frame-options").is_some());
    });
}

/// The shared app trusts 127.0.0.1, the harness peer: a proxy saying the
/// client connected over HTTPS gets HSTS on the response.
#[test]
fn forwarded_proto_from_a_trusted_proxy_gets_hsts() {
    run_test(async {
        let app = shared_app().await;
        let response = app.request(health_over_https()).await;
        assert_eq!(
            response.headers().get(HSTS).map(|v| v.to_str().unwrap()),
            Some("max-age=31536000; includeSubDomains"),
        );

        // Plain HTTP from the same trusted proxy: no HSTS.
        let response = app
            .request(
                Request::get("/health")
                    .header("x-forwarded-proto", "http")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert!(response.headers().get(HSTS).is_none());
    });
}
