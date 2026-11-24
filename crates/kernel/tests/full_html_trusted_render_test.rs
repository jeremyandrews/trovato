#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A `full_html` body renders as sanitized rich HTML, not escaped and not raw,
//! on every view path (B151): a plain item field, a compound section's
//! sub-field. Both must keep ordinary markup unescaped and strip a `<script>`,
//! an event handler and a `javascript:` URL.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use serde_json::json;
use uuid::Uuid;

const TYPE: &str = "b151_full_html";

/// Realistic rich markup plus an injected script, an `onerror` handler and a
/// `javascript:` URL: what a `full_html` author's body might hold, and what
/// an attacker might try to smuggle into it.
const PAYLOAD: &str = r#"<p class="lead">Hi <em>there</em></p><div><script>alert(1)</script></div><a href="javascript:alert(1)" onclick="x()">x</a><img src="/a.png" onerror="x()">"#;

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn ensure_type(app: &TestApp) {
    use trovato_sdk::types::{FieldDefinition, FieldType};
    let mut compound = FieldDefinition::new(
        "f_compound",
        FieldType::Compound {
            allowed_types: vec!["text".to_string()],
            min_items: None,
            max_items: None,
        },
    )
    .label("Compound");
    compound.settings = json!({"section_types": [{
        "machine_name": "text",
        "label": "Text",
        "fields": [{"field_name": "body", "field_type": "TextLong", "label": "Body", "required": true}]
    }]});
    let fields = vec![
        FieldDefinition::new("f_body", FieldType::TextLong).label("Body"),
        compound,
    ];
    let settings = json!({ "fields": serde_json::to_value(&fields).unwrap() });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, $1, '', true, 'Title', 'b151_test', $2) \
         ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
    )
    .bind(TYPE)
    .bind(&settings)
    .execute(&app.db)
    .await
    .unwrap();
    app.state
        .content_types()
        .replace(TYPE, TYPE, None, settings)
        .await
        .ok();
}

fn admin_ctx(uid: Uuid) -> trovato_kernel::tap::UserContext {
    trovato_kernel::tap::UserContext::administrator(
        uid,
        vec!["administer site".to_string(), "publish content".to_string()],
    )
}

async fn create(app: &TestApp, uid: Uuid, fields: serde_json::Value) -> Uuid {
    app.state
        .items()
        .create(
            trovato_kernel::models::CreateItem {
                item_type: TYPE.to_string(),
                title: "B151 full_html".to_string(),
                author_id: uid,
                status: Some(1),
                promote: None,
                sticky: None,
                fields: Some(fields),
                stage_id: None,
                language: None,
                log: None,
            },
            &admin_ctx(uid),
        )
        .await
        .expect("seed item")
        .id
}

fn assert_cleaned(html: &str) {
    assert!(
        html.contains(r#"<p class="lead">Hi <em>there</em></p>"#),
        "rich markup missing or escaped: {html}"
    );
    assert!(!html.contains("<script"), "script survived: {html}");
    assert!(!html.contains("onclick"), "onclick survived: {html}");
    assert!(!html.contains("onerror"), "onerror survived: {html}");
    assert!(
        !html.contains("javascript:"),
        "javascript: survived: {html}"
    );
}

// SECURITY REGRESSION TEST — B151: a full_html body renders as sanitized rich
// HTML on the item page, never escaped and never raw.
#[test]
fn a_full_html_body_renders_cleaned_on_the_item_page() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let name = common::username("b151page");
        let cookies = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple",
                &format!("{name}@test.local"),
            )
            .await;
        let uid = common::user_id_of(app, &name).await;
        let id = create(
            app,
            uid,
            json!({"f_body": {"value": PAYLOAD, "format": "full_html"}}),
        )
        .await;

        let (status, html) = get(app, &cookies, &format!("/item/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{html}");
        assert_cleaned(&html);
    });
}

// SECURITY REGRESSION TEST — B151: the same cleaning applies inside a
// compound section's full_html sub-field.
#[test]
fn a_full_html_compound_section_field_renders_cleaned() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let name = common::username("b151cmp");
        let cookies = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple",
                &format!("{name}@test.local"),
            )
            .await;
        let uid = common::user_id_of(app, &name).await;
        let id = create(
            app,
            uid,
            json!({"f_compound": {"sections": [{"type": "text", "weight": 0, "data": {
                "body": {"value": PAYLOAD, "format": "full_html"}
            }}]}}),
        )
        .await;

        let (status, html) = get(app, &cookies, &format!("/item/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{html}");
        assert_cleaned(&html);
    });
}
