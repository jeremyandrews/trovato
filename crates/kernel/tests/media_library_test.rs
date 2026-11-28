#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The media library, the media picker and usage tracking (BL-115 / B290).
//!
//! The shared test app's plugin directory does not discover real plugins
//! (the same note `media_alt_text_test.rs` and `content_lock_test.rs` make),
//! so `trovato_media`'s `tap_item_info`/`tap_media_source` never actually
//! dispatch here. This seeds the same four content types by hand
//! (`ContentTypeRegistry::replace`, as the conference type fixture does) and
//! registers them as media sources through `AppState::set_media_sources_for_test`,
//! the seam `state.rs` documents for exactly this reason.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, grant_via_role, run_test, shared_app};
use serde_json::json;
use trovato_kernel::models::CreateItem;
use trovato_kernel::plugin::media_source_registry::MediaSourceRegistry;
use trovato_kernel::tap::UserContext;
use trovato_sdk::types::{FieldDefinition, FieldType, MediaSourceDefinition};
use uuid::Uuid;

const MEDIA_FILE: &str = "media_file";
const MEDIA_IMAGE: &str = "media_image";
const MEDIA_REMOTE_VIDEO: &str = "media_remote_video";
const MEDIA_REMOTE_EMBED: &str = "media_remote_embed";

/// Register the four media content types and the media source registry a
/// real boot would build from `trovato_media`'s taps. Safe to call repeatedly
/// (every write is an upsert); safe to call from concurrent tests (every
/// caller writes the same four types and the same registry).
async fn seed_media_types(app: &TestApp) {
    let types: Vec<(&str, &str, Vec<FieldDefinition>)> = vec![
        (
            MEDIA_FILE,
            "File",
            vec![
                FieldDefinition::new("field_file", FieldType::File)
                    .required()
                    .label("File"),
            ],
        ),
        (
            MEDIA_IMAGE,
            "Image",
            vec![
                FieldDefinition::new("field_file", FieldType::File)
                    .required()
                    .label("Image"),
                FieldDefinition::new("field_alt_text", FieldType::Text { max_length: None })
                    .label("Alt text"),
                FieldDefinition::new("field_caption", FieldType::TextLong).label("Caption"),
            ],
        ),
        (
            MEDIA_REMOTE_VIDEO,
            "Remote video",
            vec![
                FieldDefinition::new("field_remote_url", FieldType::Text { max_length: None })
                    .required()
                    .label("Video URL"),
                FieldDefinition::new(
                    "field_thumbnail",
                    FieldType::RecordReference("media".into()),
                )
                .label("Thumbnail"),
            ],
        ),
        (
            MEDIA_REMOTE_EMBED,
            "Remote embed",
            vec![
                FieldDefinition::new("field_remote_url", FieldType::Text { max_length: None })
                    .required()
                    .label("Embed URL"),
                FieldDefinition::new(
                    "field_thumbnail",
                    FieldType::RecordReference("media".into()),
                )
                .label("Thumbnail"),
            ],
        ),
    ];
    for (machine, label, fields) in &types {
        let settings = json!({ "fields": serde_json::to_value(fields).unwrap() });
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, $2, '', true, 'Name', 'trovato_media', $3) \
             ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
        )
        .bind(machine)
        .bind(label)
        .bind(&settings)
        .execute(&app.db)
        .await
        .expect("seed media content type");
        app.state
            .content_types()
            .replace(machine, label, None, settings)
            .await
            .ok();
    }

    let sources: Vec<MediaSourceDefinition> = types
        .iter()
        .map(|(machine, label, _)| MediaSourceDefinition::new(*machine, *label, ""))
        .collect();
    let registry = MediaSourceRegistry::from_tap_results(vec![(
        "trovato_media".to_string(),
        serde_json::to_string(&sources).unwrap(),
    )]);
    app.state.set_media_sources_for_test(registry);
}

/// A real test user, logged in, holding exactly `permissions` — proving each
/// screen is gated on the permission, not on being an administrator (who
/// holds everything implicitly).
async fn session_with(app: &TestApp, prefix: &str, permissions: &[&str]) -> (Uuid, String) {
    let name = format!("{prefix}_{}", Uuid::now_v7().simple());
    let email = format!("{name}@example.test");
    app.create_test_user(&name, "test-password-123", &email)
        .await;
    let id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(&name)
        .fetch_one(&app.db)
        .await
        .expect("find seeded user");
    if !permissions.is_empty() {
        grant_via_role(app, id, permissions).await;
    }
    let cookies = app.login(&name, "test-password-123").await;
    (id, cookies)
}

fn user_ctx(id: Uuid, permissions: &[&str]) -> UserContext {
    UserContext::authenticated(id, permissions.iter().map(|s| s.to_string()).collect())
}

/// Percent-encode a value for an `application/x-www-form-urlencoded` body —
/// a CSRF token may contain `+` or `/`, which that encoding reads as a space
/// or leaves as a literal that breaks percent-decoding elsewhere in the body.
fn urlencode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// A fresh CSRF token: the library page's own meta tag. The token is
/// single-use (`form::csrf::verify_csrf_token` removes it once verified), so
/// a test posting more than once must fetch a new one each time.
async fn fresh_csrf(app: &TestApp, cookies: &str) -> String {
    let page = app
        .request_with_cookies(
            Request::get("/admin/content/media")
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let page = body_text(page).await;
    let marker = "name=\"csrf-token\" content=\"";
    let start = page.find(marker).expect("csrf meta tag") + marker.len();
    page[start..].split('"').next().unwrap().to_string()
}

#[test]
fn anonymous_is_refused_and_a_permitted_editor_is_admitted() {
    run_test(async {
        let app = shared_app().await;
        seed_media_types(app).await;

        for path in ["/media", "/admin/content/media"] {
            let anon = app
                .request(Request::get(path).body(Body::empty()).unwrap())
                .await;
            // `require_permission`'s rule: nobody signed in is `401` (the
            // login form), which the error page presents as "Access denied"
            // but is not the same status as a signed-in refusal.
            assert_eq!(
                anon.status(),
                StatusCode::UNAUTHORIZED,
                "{path} must refuse an anonymous visitor"
            );
        }

        let (_, cookies) = session_with(app, "medlib", &["access media overview"]).await;
        for path in ["/media", "/admin/content/media"] {
            let response = app
                .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), &cookies)
                .await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{path} must admit a holder of 'access media overview'"
            );
        }

        let (_, no_perm_cookies) = session_with(app, "noperm", &[]).await;
        let response = app
            .request_with_cookies(
                Request::get("/admin/content/media")
                    .body(Body::empty())
                    .unwrap(),
                &no_perm_cookies,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "a signed-in user without the permission must still be refused"
        );
    });
}

#[test]
fn usage_is_tracked_and_delete_is_refused_until_forced() {
    run_test(async {
        let app = shared_app().await;
        seed_media_types(app).await;
        // "delete any content" rather than "delete any media": the flat
        // permission is honoured by trovato_media's own tap_item_access,
        // which (like every plugin tap) never dispatches in this fake-plugin
        // test environment — the plugin's own unit tests already cover that
        // mapping (plugins/trovato_media/src/lib.rs). The kernel's generic
        // fallback is what is actually under test here: the delete-refusal
        // behaviour this route adds on top of it.
        let (author, cookies) = session_with(
            app,
            "medowner",
            &[
                "access media overview",
                "create media",
                "delete any content",
                "publish content",
            ],
        )
        .await;
        let ctx = user_ctx(author, &["create media", "publish content"]);

        // An image, referenced by a remote video's thumbnail field.
        let image = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: MEDIA_IMAGE.to_string(),
                    title: "A sunset".to_string(),
                    author_id: author,
                    status: Some(1),
                    promote: None,
                    sticky: None,
                    fields: Some(json!({ "field_file": Uuid::now_v7().to_string() })),
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &ctx,
            )
            .await
            .expect("create image");

        let video = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: MEDIA_REMOTE_VIDEO.to_string(),
                    title: "A talk".to_string(),
                    author_id: author,
                    status: Some(1),
                    promote: None,
                    sticky: None,
                    fields: Some(json!({
                        "field_remote_url": "https://example.test/video",
                        "field_thumbnail": image.id.to_string(),
                    })),
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &ctx,
            )
            .await
            .expect("create video");

        let library = app
            .request_with_cookies(
                Request::get("/admin/content/media")
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(library.status(), StatusCode::OK);
        let library = body_text(library).await;
        assert!(
            library.contains("A sunset") && library.contains("A talk"),
            "both items must be listed: {library}"
        );

        // Delete without force: the image is in use, so it must survive.
        let csrf = fresh_csrf(app, &cookies).await;
        let refused = app
            .request_with_cookies(
                Request::post(format!("/admin/content/media/{}/delete", image.id))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!("_token={}", urlencode(&csrf))))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert!(refused.status().is_redirection(), "{:?}", refused.status());
        assert!(
            app.state.items().load(image.id).await.unwrap().is_some(),
            "a media item in use must not be deleted without force"
        );

        // Force delete removes it.
        let csrf = fresh_csrf(app, &cookies).await;
        let forced = app
            .request_with_cookies(
                Request::post(format!("/admin/content/media/{}/delete", image.id))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!("_token={}&force=1", urlencode(&csrf))))
                    .unwrap(),
                &cookies,
            )
            .await;
        let forced_status = forced.status();
        let forced_body = body_text(forced).await;
        assert!(
            forced_status.is_redirection(),
            "{forced_status:?}: {forced_body}"
        );
        assert!(
            app.state.items().load(image.id).await.unwrap().is_none(),
            "force delete must remove the media item"
        );

        // The video (never in use) deletes on the first try.
        let csrf = fresh_csrf(app, &cookies).await;
        let deleted = app
            .request_with_cookies(
                Request::post(format!("/admin/content/media/{}/delete", video.id))
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!("_token={}", urlencode(&csrf))))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert!(deleted.status().is_redirection(), "{:?}", deleted.status());
        assert!(app.state.items().load(video.id).await.unwrap().is_none());
    });
}

#[test]
fn the_type_filter_narrows_the_listing() {
    run_test(async {
        let app = shared_app().await;
        seed_media_types(app).await;
        let (author, cookies) = session_with(
            app,
            "medfilter",
            &["access media overview", "create media", "publish content"],
        )
        .await;
        let ctx = user_ctx(author, &["create media", "publish content"]);

        app.state
            .items()
            .create(
                CreateItem {
                    item_type: MEDIA_FILE.to_string(),
                    title: "filter-only-file-item".to_string(),
                    author_id: author,
                    status: Some(1),
                    promote: None,
                    sticky: None,
                    fields: Some(json!({ "field_file": Uuid::now_v7().to_string() })),
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &ctx,
            )
            .await
            .expect("create file item");
        app.state
            .items()
            .create(
                CreateItem {
                    item_type: MEDIA_REMOTE_EMBED.to_string(),
                    title: "filter-only-embed-item".to_string(),
                    author_id: author,
                    status: Some(1),
                    promote: None,
                    sticky: None,
                    fields: Some(json!({ "field_remote_url": "https://example.test/embed" })),
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &ctx,
            )
            .await
            .expect("create embed item");

        let filtered = app
            .request_with_cookies(
                Request::get("/admin/content/media?type=media_file")
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(filtered.status(), StatusCode::OK);
        let filtered = body_text(filtered).await;
        assert!(filtered.contains("filter-only-file-item"), "{filtered}");
        assert!(!filtered.contains("filter-only-embed-item"), "{filtered}");
    });
}

#[test]
fn the_picker_lists_media_and_uploads_a_new_image() {
    run_test(async {
        let app = shared_app().await;
        seed_media_types(app).await;
        let (author, cookies) = session_with(
            app,
            "medpicker",
            &["access media overview", "create media", "publish content"],
        )
        .await;
        let ctx = user_ctx(author, &["create media", "publish content"]);

        app.state
            .items()
            .create(
                CreateItem {
                    item_type: MEDIA_FILE.to_string(),
                    title: "picker-visible-item".to_string(),
                    author_id: author,
                    status: Some(1),
                    promote: None,
                    sticky: None,
                    fields: Some(json!({ "field_file": Uuid::now_v7().to_string() })),
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &ctx,
            )
            .await
            .expect("create file item");

        let listed = app
            .request_with_cookies(
                Request::get("/admin/content/media/picker")
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(listed.status(), StatusCode::OK);
        let listed: serde_json::Value = serde_json::from_str(&body_text(listed).await).unwrap();
        let items = listed["items"].as_array().unwrap();
        assert!(
            items.iter().any(|i| i["label"] == "picker-visible-item"),
            "{items:?}"
        );

        // Upload new, through the dialog's own endpoint: a remote video this
        // time, which needs no file bytes.
        let csrf_page = app
            .request_with_cookies(
                Request::get("/admin/content/media")
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let csrf_page = body_text(csrf_page).await;
        let marker = "name=\"csrf-token\" content=\"";
        let start = csrf_page.find(marker).expect("csrf meta tag") + marker.len();
        let csrf = csrf_page[start..].split('"').next().unwrap().to_string();

        let boundary = "b290-test-boundary";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"media_type\"\r\n\r\n{MEDIA_REMOTE_EMBED}\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\npicker-uploaded-embed\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"remote_url\"\r\n\r\nhttps://example.test/new\r\n\
             --{boundary}--\r\n"
        );
        let response = app
            .request_with_cookies(
                Request::post("/admin/content/media/picker/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .header("X-CSRF-Token", csrf)
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(parsed["success"], true, "{parsed}");
        assert_eq!(parsed["item"]["label"], "picker-uploaded-embed");

        let created_id: Uuid = parsed["item"]["id"].as_str().unwrap().parse().unwrap();
        let item = app.state.items().load(created_id).await.unwrap().unwrap();
        assert_eq!(item.item_type, MEDIA_REMOTE_EMBED);
        assert_eq!(
            item.fields.get("field_remote_url").and_then(|v| v.as_str()),
            Some("https://example.test/new")
        );
    });
}
