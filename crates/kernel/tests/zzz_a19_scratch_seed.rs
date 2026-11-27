#![allow(clippy::unwrap_used, clippy::expect_used)]
//! SCRATCH: seeds a page for manual browser verification of A19 K5/K7/K8/K9.
//! Not part of the suite — deleted before the final commit.

mod common;

use common::{TestApp, run_test, shared_app};
use serde_json::json;
use trovato_kernel::models::CreateItem;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const TYPE: &str = "a19_verify_page";

fn admin(uid: Uuid) -> UserContext {
    UserContext::administrator(
        uid,
        vec!["administer site".to_string(), "publish content".to_string()],
    )
}

async fn ensure_type(app: &TestApp) {
    use trovato_sdk::types::{FieldDefinition, FieldType};
    let fields = vec![FieldDefinition::new("f_layout", FieldType::PageBuilder).label("Layout")];
    let settings = json!({ "fields": serde_json::to_value(&fields).unwrap() });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, $2, '', true, 'Title', 'a19_scratch', $3) \
         ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
    )
    .bind(TYPE)
    .bind("A19 Verify Page")
    .bind(&settings)
    .execute(&app.db)
    .await
    .unwrap();
    app.state
        .content_types()
        .replace(TYPE, "A19 Verify Page", None, settings)
        .await
        .ok();
}

fn page() -> serde_json::Value {
    json!({
        "root": { "title": "A19 Verification Page" },
        "content": [
            { "type": "Hero", "props": {
                "title": "A19 Verification Page",
                "subtitle": "K5 page H1 + K7 embeds + K8 sanitizer + K9 alt check",
                "variant": "standard",
                "headingLevel": 1
            } },
            { "type": "YouTubeEmbed", "props": {
                "videoId": "dQw4w9WgXcQ",
                "title": "An allowed YouTube embed"
            } },
            { "type": "Accordion", "props": {
                "items": [
                    { "title": "Does the accordion keep its id/aria-*?", "content": "Yes, after K8." }
                ]
            } },
            { "type": "BlockquoteExtended", "props": {
                "text": "A decorative avatar must not block the page.",
                "attribution": "A19 Verifier",
                "imageUrl": "https://www.gstatic.com/webp/gallery/1.jpg",
                "isDecorative": true
            } }
        ]
    })
}

#[test]
fn seed_the_verification_page() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let uid = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO users (id, name, pass, mail, status, is_admin) \
             VALUES ($1, $2, 'x', $3, 1, false) ON CONFLICT (id) DO NOTHING",
        )
        .bind(uid)
        .bind(format!("a19owner{}", uid.simple()))
        .bind(format!("a19-{}@example.test", uid.simple()))
        .execute(&app.db)
        .await
        .unwrap();

        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: TYPE.to_string(),
                    title: "A19 Verification Page".to_string(),
                    author_id: uid,
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(json!({ "f_layout": page() })),
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &admin(uid),
            )
            .await
            .expect("the verification page saves");
        println!("A19_SEED_ITEM_ID={}", item.id);
        println!("A19_SEED_URL=/item/{}", item.id);
    });
}
