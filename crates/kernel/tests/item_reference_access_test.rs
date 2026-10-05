#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Problem 3 — the item page's reference titles are filtered.
//!
//! `view_item` resolves every RecordReference field's target and every item
//! referring back, and puts both lists — id, title and type — into the template
//! context as `referenced_items` and `reverse_references`. It resolved them with
//! `ItemService::load` and `find_referencing`, neither of which asks anybody
//! anything: the viewer had passed `check_access` for the page's own item, which
//! says nothing about its neighbours. A reference to a draft therefore rendered
//! that draft's title to anyone who could see the referring page, and a draft
//! referring to a published item named itself on that item's page.
//!
//! # Why this file has its own app
//!
//! The shipped theme renders neither context key, so there is no page in the
//! repository on which the leak is observable; `docs/tutorial/templates/` is the
//! documented example of a theme that reads them. This file therefore builds its
//! own `TestApp` with `crates/kernel/tests/fixtures/templates/` overlaid on the
//! real template directory, which is the smallest thing that puts both lists on
//! a page.
//!
//! Requires Postgres + Redis.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::TestApp;
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Two probe types, one of which points at the other.
const REFERRER: &str = "seam_ref_source";
const TARGET: &str = "seam_ref_target";

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("reference fixture app init thread panicked")
    })
}

async fn build_app() -> TestApp {
    let project_root = common::project_root();
    TestApp::with_config(|config| {
        // The fixture directory last, so its two templates override nothing and
        // are simply added to the search path.
        config.templates_dirs = vec![
            project_root.join("templates"),
            project_root.join("crates/kernel/tests/fixtures/templates"),
        ];
    })
    .await
}

async fn ensure_reference_types(app: &TestApp) {
    use trovato_sdk::types::{FieldDefinition, FieldType};

    for (name, label, fields) in [
        (
            TARGET,
            "Seam Reference Target",
            vec![
                FieldDefinition::new("field_note", FieldType::Text { max_length: None })
                    .label("Note"),
            ],
        ),
        (
            REFERRER,
            "Seam Reference Source",
            vec![
                FieldDefinition::new(
                    "field_target",
                    FieldType::RecordReference(TARGET.to_string()),
                )
                .label("Target"),
            ],
        ),
    ] {
        let settings = serde_json::json!({ "fields": serde_json::to_value(&fields).unwrap() });
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, $2, '', true, 'Title', 'seam_test', $3) \
             ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
        )
        .bind(name)
        .bind(label)
        .bind(&settings)
        .execute(&app.db)
        .await
        .expect("seed probe type");

        app.state
            .content_types()
            .create(name, label, None, settings)
            .await
            .ok();
    }
}

async fn make_item(
    app: &TestApp,
    item_type: &str,
    title: &str,
    status: i16,
    fields: serde_json::Value,
) -> Uuid {
    let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
    app.state
        .items()
        .create(
            CreateItem {
                item_type: item_type.to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(fields),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("reference access test".to_string()),
            },
            &admin,
        )
        .await
        .expect("create")
        .id
}

async fn page_of(app: &TestApp, id: Uuid, bucket: &str) -> String {
    let response = app
        .request(
            Request::get(format!("/item/{id}"))
                .header("x-forwarded-for", common::test_ip_for(bucket))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK, "the page must render");
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[test]
fn forward_reference_titles_are_filtered() {
    common::run_test(async {
        let app = app();
        ensure_reference_types(app).await;

        let tag = Uuid::now_v7().simple().to_string();
        let hidden = make_item(
            app,
            TARGET,
            &format!("Hidden Target {tag}"),
            0,
            serde_json::json!({}),
        )
        .await;
        let visible = make_item(
            app,
            TARGET,
            &format!("Visible Target {tag}"),
            1,
            serde_json::json!({}),
        )
        .await;

        let hides = make_item(
            app,
            REFERRER,
            &format!("Refers To Hidden {tag}"),
            1,
            serde_json::json!({ "field_target": hidden.to_string() }),
        )
        .await;
        let shows = make_item(
            app,
            REFERRER,
            &format!("Refers To Visible {tag}"),
            1,
            serde_json::json!({ "field_target": visible.to_string() }),
        )
        .await;

        let html = page_of(app, shows, "refseam-a").await;
        assert!(
            html.contains(&format!("Visible Target {tag}")),
            "a published target's title is still rendered, got {html}"
        );

        let html = page_of(app, hides, "refseam-b").await;
        assert!(
            !html.contains(&format!("Hidden Target {tag}")),
            "an unpublished target's title must not reach the page, got {html}"
        );
    });
}

#[test]
fn reverse_reference_titles_are_filtered() {
    common::run_test(async {
        let app = app();
        ensure_reference_types(app).await;

        let tag = Uuid::now_v7().simple().to_string();
        let target = make_item(
            app,
            TARGET,
            &format!("Reverse Target {tag}"),
            1,
            serde_json::json!({}),
        )
        .await;

        let published = make_item(
            app,
            REFERRER,
            &format!("Published Referrer {tag}"),
            1,
            serde_json::json!({ "field_target": target.to_string() }),
        )
        .await;
        let draft = make_item(
            app,
            REFERRER,
            &format!("Draft Referrer {tag}"),
            0,
            serde_json::json!({ "field_target": target.to_string() }),
        )
        .await;
        assert_ne!(published, draft);

        let html = page_of(app, target, "refseam-c").await;
        assert!(
            html.contains(&format!("Published Referrer {tag}")),
            "a published referrer is still listed, got {html}"
        );
        assert!(
            !html.contains(&format!("Draft Referrer {tag}")),
            "an unpublished referrer's title must not reach the page, got {html}"
        );
    });
}
