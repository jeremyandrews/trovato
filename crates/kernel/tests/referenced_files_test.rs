#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The item page resolves File fields into `referenced_files`.
//!
//! A File field stores a `file_managed` uuid, and nothing on the item route
//! turned that uuid into a URL, so a template could not link the file it names
//! (Ritrovo's templates linked `/file/<uuid>/download`, a route the kernel does
//! not have). `referenced_files` is keyed by field name; each entry carries the
//! file's id, its `/files/{path}` URL, filename, stored type and alt text.
//!
//! Like `item_reference_access_test`, this builds its own app with
//! `crates/kernel/tests/fixtures/templates/` overlaid, because the shipped
//! theme renders no such key.
//!
//! Requires Postgres + Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::TestApp;
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use trovato_sdk::types::{FieldDefinition, FieldType};
use uuid::Uuid;

const PROBE: &str = "b72_file_probe";

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("file probe app init thread panicked")
    })
}

async fn build_app() -> TestApp {
    let project_root = common::project_root();
    TestApp::with_config(|config| {
        config.templates_dirs = vec![
            project_root.join("templates"),
            project_root.join("crates/kernel/tests/fixtures/templates"),
        ];
    })
    .await
}

async fn ensure_probe_type(app: &TestApp) {
    let mut gallery = FieldDefinition::new("field_gallery", FieldType::File).label("Gallery");
    gallery.cardinality = -1;
    let fields = vec![
        FieldDefinition::new("field_programme", FieldType::File).label("Programme"),
        gallery,
    ];
    let settings = serde_json::json!({ "fields": serde_json::to_value(&fields).unwrap() });
    app.state
        .content_types()
        .replace(PROBE, "File Probe", None, settings)
        .await
        .expect("seed probe type");
}

fn png() -> Vec<u8> {
    let img = image::RgbImage::from_fn(4, 4, |x, y| image::Rgb([x as u8 * 60, y as u8 * 60, 0]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[test]
fn file_fields_resolve_to_urls_on_the_item_page() {
    common::run_test(async {
        let app = app();
        ensure_probe_type(app).await;

        let files = app.state.files();
        let pdf = files
            .upload(
                Uuid::nil(),
                "programme.pdf",
                "application/pdf",
                b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF\n",
            )
            .await
            .expect("upload pdf");
        let photo = files
            .upload(Uuid::nil(), "stage.png", "image/png", &png())
            .await
            .expect("upload png");
        files
            .set_alt_text(photo.id, Some("The main stage"))
            .await
            .unwrap();
        let missing = Uuid::now_v7();

        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: PROBE.to_string(),
                    title: "File Probe Item".to_string(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({
                        "field_programme": pdf.id.to_string(),
                        "field_gallery": [photo.id.to_string(), missing.to_string()],
                    })),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: None,
                },
                &admin,
            )
            .await
            .expect("create");

        let response = app
            .request(
                Request::get(format!("/item/{}", item.id))
                    .header("x-forwarded-for", common::test_ip_for("b72_file_probe"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&bytes);

        let pdf_url = format!("/files/{}", pdf.uri.strip_prefix("local://").unwrap());
        let photo_url = format!("/files/{}", photo.uri.strip_prefix("local://").unwrap());
        assert!(
            html.contains(&format!(
                r#"data-field="field_programme" data-id="{}" data-mime="application/pdf" data-alt="" href="{pdf_url}">programme.pdf</a>"#,
                pdf.id
            )),
            "the PDF field resolves to its URL, type and name:\n{html}"
        );
        assert!(
            html.contains(&format!(
                r#"data-field="field_gallery" data-id="{}" data-mime="image/png" data-alt="The main stage" href="{photo_url}">stage.png</a>"#,
                photo.id
            )),
            "the gallery image resolves with its alt text:\n{html}"
        );
        assert!(
            !html.contains(&missing.to_string()),
            "a uuid naming no file resolves to nothing"
        );
    });
}
