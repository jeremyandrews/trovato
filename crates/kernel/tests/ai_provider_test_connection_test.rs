#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The AI provider "Test" button route (B300) answers JSON in every case,
//! including a body the form extractor itself refuses.
//!
//! `static/js/ai-admin.js` used to submit `.js-test-connection` with
//! `fetch(form.action, { body: new FormData(form) })`, which a browser sends
//! as `multipart/form-data`. The route took `Form(form): Form<CsrfOnlyForm>`,
//! which only accepts `application/x-www-form-urlencoded`: axum rejected the
//! multipart body before the handler ran, with a plain text rejection body,
//! so the script's `data.latency_ms` read as `undefined` ("Latency:
//! undefinedms") despite the handler's own doc comment promising JSON in all
//! cases.
//!
//! The fix is two-sided: the script now sends the body urlencoded, and the
//! route extracts `Form` by hand so that even a wrongly encoded body answers
//! in the same `{success, message, latency_ms}` shape the handler's other
//! failure branches already use.
//!
//! These tests hit the route directly, as a client would, rather than the
//! handler function: one posts the urlencoded body the fixed script now
//! sends, the other posts the multipart body the old script sent (and a
//! misbehaving or out of date client still could), proving both land on the
//! same JSON contract.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{run_test, shared_app};

/// Pull the page's `_token` CSRF field out of its HTML, the way a form on
/// the page would submit it. The session accepts any token it issued, from
/// any page, so the add provider form's own token (rendered whether or not
/// any provider row exists yet) works against the test route too.
fn csrf_token(html: &str) -> String {
    let needle = r#"name="_token" value=""#;
    let at = html.find(needle).expect("a _token field on the page") + needle.len();
    html[at..at + html[at..].find('"').unwrap()].to_string()
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "response body was not JSON ({e}): {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

#[test]
fn a_urlencoded_post_the_fixed_script_now_sends_reaches_the_handler() {
    run_test(async {
        let app = shared_app().await;
        let name = common::username("b300urlencoded");
        let cookies = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple",
                &format!("{name}@test.local"),
            )
            .await;

        let page = app
            .request_with_cookies(
                Request::get("/admin/system/ai-providers/add")
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let html = String::from_utf8_lossy(
            &axum::body::to_bytes(page.into_body(), 5_000_000)
                .await
                .unwrap(),
        )
        .to_string();
        let token = csrf_token(&html);

        let form = format!("_token={}", urlencoding::encode(&token));
        let response = app
            .request_with_cookies(
                Request::post("/admin/system/ai-providers/no-such-provider/test")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(form))
                    .unwrap(),
                &cookies,
            )
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        // Reaching the handler's provider lookup (rather than a CSRF or
        // extractor refusal) proves the urlencoded body was read.
        assert_eq!(json["success"], false);
        assert_eq!(json["message"], "Provider not found.");
        assert_eq!(json["latency_ms"], 0);
    });
}

#[test]
fn a_multipart_post_the_old_script_sent_still_gets_the_json_failure_shape() {
    run_test(async {
        let app = shared_app().await;
        let name = common::username("b300multipart");
        let cookies = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple",
                &format!("{name}@test.local"),
            )
            .await;

        let page = app
            .request_with_cookies(
                Request::get("/admin/system/ai-providers/add")
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let html = String::from_utf8_lossy(
            &axum::body::to_bytes(page.into_body(), 5_000_000)
                .await
                .unwrap(),
        )
        .to_string();
        let token = csrf_token(&html);

        // Exactly what `new FormData(form)` produced before the fix: a
        // multipart body carrying the same `_token` field.
        let boundary = "b300TestBoundary";
        let multipart_body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"_token\"\r\n\r\n{token}\r\n--{boundary}--\r\n"
        );

        let response = app
            .request_with_cookies(
                Request::post("/admin/system/ai-providers/no-such-provider/test")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(multipart_body))
                    .unwrap(),
                &cookies,
            )
            .await;

        // Never axum's plain text rejection: the same shape every other
        // failure in this handler answers with.
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["success"], false);
        assert!(
            json["message"].as_str().is_some_and(|m| !m.is_empty()),
            "a human readable message: {json}"
        );
        assert!(
            json["latency_ms"].is_number(),
            "latency_ms must be a number, never missing: {json}"
        );
        // The rejection happens before the provider lookup: this is not the
        // "Provider not found." branch.
        assert_ne!(json["message"], "Provider not found.");
    });
}
