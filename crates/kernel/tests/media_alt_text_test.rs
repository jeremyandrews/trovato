#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Alternative text on managed files.
//!
//! Media had no alt field, so `templates/form/file-upload.html`,
//! `templates/admin/media-library.html` and `templates/admin/file-details.html`
//! all rendered `alt="{{ file.filename }}"`. A filename is not alternative text:
//! at best it is noise read aloud, at worst it is "IMG_4821.jpg" standing in for
//! the content of an image (WCAG F30). The block editor already did this right,
//! including the decorative case; the media entity now can too.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, extract_cookies, run_test, shared_app};
use uuid::Uuid;

/// A 1x1 PNG, so uploads pass the magic-byte check.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

/// Upload an image owned by an existing user and return its id.
async fn upload_image(app: &TestApp, owner: Uuid, filename: &str) -> Uuid {
    app.state
        .files()
        .upload(owner, filename, "image/png", PNG)
        .await
        .expect("upload image")
        .id
}

/// An admin's session cookies and a CSRF token that goes with them.
async fn admin_session(app: &TestApp) -> (String, String, Uuid) {
    let name = format!("altadmin_{}", Uuid::now_v7().simple());
    app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let cookies = app.login(&name, "test-password-123").await;

    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(&name)
        .fetch_one(&app.db)
        .await
        .expect("find admin");

    let response = app
        .request_with_cookies(
            Request::get("/admin/content/files")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;
    let new_cookies = extract_cookies(&response);
    let cookies = if new_cookies.is_empty() {
        cookies
    } else {
        new_cookies
    };
    let html = text_of(response).await;
    let marker = "name=\"csrf-token\" content=\"";
    let start = html.find(marker).expect("csrf meta tag") + marker.len();
    let token = html[start..].split('"').next().unwrap().to_string();

    (cookies, token, user_id)
}

async fn text_of(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

fn urlencode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// A freshly uploaded file has no alt text — not the filename, not an empty
/// string. Nobody has said what the image shows yet, and that is different from
/// saying it shows nothing.
#[test]
fn a_new_upload_has_no_alt_text_rather_than_a_filename() {
    run_test(async {
        let app = shared_app().await;
        let (_, _, owner) = admin_session(app).await;

        let file_id = upload_image(app, owner, "IMG_4821.png").await;
        let file = app
            .state
            .files()
            .get(file_id)
            .await
            .expect("load file")
            .expect("file exists");

        assert_eq!(
            file.alt_text, None,
            "an upload must not be given the filename as alt text"
        );
    });
}

/// Alt text round-trips, and the two ways of having none stay distinct.
#[test]
fn alt_text_round_trips_and_decorative_is_distinct_from_unset() {
    run_test(async {
        let app = shared_app().await;
        let (_, _, owner) = admin_session(app).await;
        let file_id = upload_image(app, owner, "photo.png").await;

        // Set it.
        assert!(
            app.state
                .files()
                .set_alt_text(file_id, Some("A cat asleep on a warm laptop"))
                .await
                .expect("set alt text")
        );
        let file = app.state.files().get(file_id).await.unwrap().unwrap();
        assert_eq!(
            file.alt_text.as_deref(),
            Some("A cat asleep on a warm laptop")
        );

        // Explicitly decorative: the empty string, not NULL.
        app.state
            .files()
            .set_alt_text(file_id, Some("   "))
            .await
            .expect("mark decorative");
        let file = app.state.files().get(file_id).await.unwrap().unwrap();
        assert_eq!(
            file.alt_text.as_deref(),
            Some(""),
            "whitespace-only means decorative, which is an answer"
        );

        // Back to "nobody has said".
        app.state
            .files()
            .set_alt_text(file_id, None)
            .await
            .expect("clear alt text");
        let file = app.state.files().get(file_id).await.unwrap().unwrap();
        assert_eq!(file.alt_text, None);
    });
}

/// Setting alt text on a file that does not exist reports that, rather than
/// silently succeeding.
#[test]
fn setting_alt_text_on_a_missing_file_reports_it() {
    run_test(async {
        let app = shared_app().await;

        let updated = app
            .state
            .files()
            .set_alt_text(Uuid::now_v7(), Some("nothing"))
            .await
            .expect("query runs");

        assert!(!updated, "no row means no update");
    });
}

/// The admin form saves alt text, and the details page renders it in the preview
/// — the assertion the defect fails: the filename must not be the alt value.
#[test]
fn the_admin_form_saves_alt_text_and_the_preview_uses_it() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, token, owner) = admin_session(app).await;
        let filename = format!("beach-{}.png", Uuid::now_v7().simple());
        let file_id = upload_image(app, owner, &filename).await;

        let alt = "Waves breaking on a pebble beach at dusk";
        let body = format!("_token={}&alt_text={}", urlencode(&token), urlencode(alt));
        let response = app
            .request_with_cookies(
                Request::post(format!("/admin/content/files/{file_id}/alt-text"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::SEE_OTHER,
            "the form must save and redirect"
        );

        let page = app
            .request_with_cookies(
                Request::get(format!("/admin/content/files/{file_id}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let page = text_of(page).await;

        assert!(
            page.contains(&format!("alt=\"{alt}\"")),
            "the preview must use the recorded alt text, page was:\n{page}"
        );
        assert!(
            !page.contains(&format!("alt=\"{filename}\"")),
            "the filename must never be the alt text"
        );
        assert!(
            page.contains(&format!("value=\"{alt}\"")),
            "the form must show the current value for editing"
        );
    });
}

/// With no alt text recorded, the preview emits `alt=""` rather than the
/// filename. The filename is displayed as text on the page already, so repeating
/// it in the alt is noise, and an empty alt is the correct markup for a
/// decorative image.
#[test]
fn a_file_without_alt_text_renders_an_empty_alt() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, _, owner) = admin_session(app).await;
        let filename = format!("unlabelled-{}.png", Uuid::now_v7().simple());
        let file_id = upload_image(app, owner, &filename).await;

        let page = app
            .request_with_cookies(
                Request::get(format!("/admin/content/files/{file_id}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let page = text_of(page).await;

        assert!(
            page.contains("alt=\"\""),
            "an image with no recorded alt text must render an empty alt, page was:\n{page}"
        );
        assert!(
            !page.contains(&format!("alt=\"{filename}\"")),
            "the filename must not be used as alt text"
        );
        assert!(
            page.contains("Not set."),
            "the form must say the alt text has never been set"
        );
    });
}

/// The media library shows which images still need alt text, which is the thing
/// a library view can usefully tell you at a glance.
#[test]
fn the_media_library_flags_images_missing_alt_text() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, _, owner) = admin_session(app).await;
        let file_id =
            upload_image(app, owner, &format!("lib-{}.png", Uuid::now_v7().simple())).await;
        // The library lists permanent media only.
        app.state
            .files()
            .mark_permanent(file_id)
            .await
            .expect("mark permanent");

        let page = app
            .request_with_cookies(
                Request::get("/admin/media").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(page.status(), StatusCode::OK);
        let page = text_of(page).await;

        assert!(
            page.contains("No alt text"),
            "the library must flag an image with no alt text, page was:\n{page}"
        );

        app.state
            .files()
            .set_alt_text(file_id, Some(""))
            .await
            .expect("mark decorative");

        let page = app
            .request_with_cookies(
                Request::get("/admin/media").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        let page = text_of(page).await;
        assert!(
            page.contains("Decorative"),
            "an image marked decorative must read as decided, not as missing"
        );
    });
}

/// The alt-text endpoint is an admin write, so it refuses a request without a
/// valid CSRF token.
#[test]
fn the_alt_text_endpoint_requires_a_csrf_token() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, _, owner) = admin_session(app).await;
        let file_id = upload_image(app, owner, "csrf.png").await;

        let response = app
            .request_with_cookies(
                Request::post(format!("/admin/content/files/{file_id}/alt-text"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("_token=not-a-real-token&alt_text=nope"))
                    .unwrap(),
                &cookies,
            )
            .await;

        assert_ne!(
            response.status(),
            StatusCode::SEE_OTHER,
            "a bad token must not save"
        );

        let file = app.state.files().get(file_id).await.unwrap().unwrap();
        assert_eq!(
            file.alt_text, None,
            "nothing may be written without a token"
        );
    });
}
