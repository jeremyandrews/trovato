#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Scalar fields on the item page, and the comment block under it.
//!
//! The item view rendered a plain string, number or boolean field as
//! `<strong class="field__label">label</strong>: value`, with the label taken
//! from the machine name. The theme hid every `.field__label`, so the label went
//! and the `: ` stayed: a person Item with two booleans read `: true` / `: false`
//! with nothing to say what either was. The label now comes from the item type's
//! field definition and carries its own separator.
//!
//! The comment block's Cancel button called `cancelReply()` from an inline
//! `onclick`, which the kernel's CSP (`script-src` with no `'unsafe-inline'`)
//! blocks, so it raised a console error and did nothing.
//!
//! The page test requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const ITEM_TYPE: &str = "scalar_label_test";

async fn ensure_item_type(app: &TestApp) {
    use trovato_sdk::types::{FieldDefinition, FieldType};

    let fields = vec![
        FieldDefinition::new("field_notify_arrive", FieldType::Boolean).label("Notify on arrival"),
        FieldDefinition::new("field_home_city", FieldType::Text { max_length: None })
            .label("Home city"),
    ];
    let settings = serde_json::json!({ "fields": serde_json::to_value(&fields).unwrap() });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'Scalar Label Test', '', true, 'Title', 'core', $2) \
         ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
    )
    .bind(ITEM_TYPE)
    .bind(&settings)
    .execute(&app.db)
    .await
    .expect("seed item type");

    app.state
        .content_types()
        .replace(ITEM_TYPE, "Scalar Label Test", None, settings)
        .await
        .ok();
}

/// A boolean and a string field render under the labels the type declares,
/// and no value is left after a bare `: ` whose label a theme can hide.
#[test]
fn scalar_fields_render_with_their_declared_label() {
    run_test(async {
        let app = shared_app().await;
        ensure_item_type(app).await;

        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
        let id = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: ITEM_TYPE.to_string(),
                    title: format!("Scalar Labels {}", Uuid::now_v7().simple()),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({
                        "field_notify_arrive": true,
                        "field_home_city": "Portland",
                    })),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("scalar label test".to_string()),
                },
                &admin,
            )
            .await
            .expect("create item")
            .id;

        let response = app
            .request(
                Request::get(format!("/item/{id}"))
                    .header("x-forwarded-for", common::test_ip_for("scalar_label"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK, "GET /item/{id}");
        let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("read body");
        let html = String::from_utf8_lossy(&bytes);

        assert!(
            html.contains(
                "<div class=\"field field-field_notify_arrive\">\
                 <strong class=\"field__label\">Notify on arrival:</strong> true</div>"
            ),
            "the boolean renders under its declared label: {html}"
        );
        assert!(
            html.contains(
                "<div class=\"field field-field_home_city\">\
                 <strong class=\"field__label\">Home city:</strong> Portland</div>"
            ),
            "the string renders under its declared label: {html}"
        );
        assert!(
            !html.contains("</strong>: "),
            "no separator is left outside a label a theme may hide: {html}"
        );
        assert!(
            !html.contains("notify arrive"),
            "the machine-name label is not used when the type declares one: {html}"
        );
    });
}

/// The comment block carries no inline event handler, which the kernel's CSP
/// would block, and the script no longer exports a global for one.
#[test]
fn the_comment_block_has_no_inline_event_handlers() {
    let root = common::project_root();
    let template = std::fs::read_to_string(root.join("templates/elements/comments.html"))
        .expect("read comments.html");
    let lower = template.to_ascii_lowercase();
    for handler in [
        "onclick=",
        "onsubmit=",
        "onchange=",
        "oninput=",
        "onkeydown=",
    ] {
        assert!(
            !lower.contains(handler),
            "comments.html must not use an inline {handler} handler"
        );
    }

    let script = std::fs::read_to_string(root.join("static/js/comment-reply.js"))
        .expect("read comment-reply.js");
    assert!(
        !script.contains("window.cancelReply"),
        "comment-reply.js binds Cancel itself and exposes no global for an inline handler"
    );
    assert!(
        script.contains(".cancel-reply"),
        "comment-reply.js binds the Cancel button"
    );
}
