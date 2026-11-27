#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A19 K11: a page with an unknown component is refused on every save path —
//! create, update and import — because every one of them runs
//! `ItemService::validate_page_builder_fields` (create, update) or
//! `ItemService::check_imported_fields` (import), both of which call
//! `ComponentRegistry::validate_page` before anything is written.

mod common;

use common::{TestApp, run_test, shared_app};
use serde_json::{Value, json};
use trovato_kernel::content::PageBuilderInvalid;
use trovato_kernel::import::{ImportFailed, import};
use trovato_kernel::models::{CreateItem, UpdateItem};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const TYPE: &str = "k11_pb_save_paths";

fn admin(uid: Uuid) -> UserContext {
    UserContext::administrator(
        uid,
        vec!["administer site".to_string(), "publish content".to_string()],
    )
}

/// A real user row: `item.author_id` has a FK to `users`. Removed when the
/// test ends.
async fn create_user(app: &TestApp) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, name, pass, mail, status, is_admin) \
         VALUES ($1, $2, 'x', $3, 1, false)",
    )
    .bind(id)
    .bind(format!("k11owner{}", id.simple()))
    .bind(format!("k11-{}@example.test", id.simple()))
    .execute(&app.db)
    .await
    .expect("seed owner user");
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

async fn ensure_type(app: &TestApp) {
    use trovato_sdk::types::{FieldDefinition, FieldType};
    let fields = vec![FieldDefinition::new("f_layout", FieldType::PageBuilder).label("Layout")];
    let settings = json!({ "fields": serde_json::to_value(&fields).unwrap() });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, $2, '', true, 'Title', 'k11_test', $3) \
         ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
    )
    .bind(TYPE)
    .bind("K11 Page Builder Save Paths")
    .bind(&settings)
    .execute(&app.db)
    .await
    .unwrap();
    app.state
        .content_types()
        .replace(TYPE, "K11 Page Builder Save Paths", None, settings)
        .await
        .ok();
}

fn valid_page() -> Value {
    json!({ "root": {}, "content": [] })
}

fn invalid_page() -> Value {
    json!({ "root": {}, "content": [ { "type": "NoSuchComponent", "props": {} } ] })
}

fn unknown_component_error(err: &anyhow::Error) -> bool {
    let invalid = err
        .downcast_ref::<PageBuilderInvalid>()
        .unwrap_or_else(|| panic!("not a PageBuilderInvalid: {err:#}"));
    invalid
        .0
        .iter()
        .any(|(_, e)| e.message.contains("unknown component type"))
}

#[test]
fn create_refuses_an_unknown_component() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let uid = create_user(app).await;

        let err = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: TYPE.to_string(),
                    title: format!("K11 create {}", Uuid::now_v7()),
                    author_id: uid,
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(json!({ "f_layout": invalid_page() })),
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &admin(uid),
            )
            .await
            .expect_err("an unknown component is refused on create");
        assert!(unknown_component_error(&err), "{err:#}");
    });
}

#[test]
fn update_refuses_an_unknown_component() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let uid = create_user(app).await;

        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: TYPE.to_string(),
                    title: format!("K11 update {}", Uuid::now_v7()),
                    author_id: uid,
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(json!({ "f_layout": valid_page() })),
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &admin(uid),
            )
            .await
            .expect("a valid page saves");
        let item_id = item.id;
        let pool = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("UPDATE item SET current_revision_id = NULL WHERE id = $1")
                .bind(item_id)
                .execute(&pool)
                .await;
            let _ = sqlx::query("DELETE FROM item_revision WHERE item_id = $1")
                .bind(item_id)
                .execute(&pool)
                .await;
            let _ = sqlx::query("DELETE FROM item WHERE id = $1")
                .bind(item_id)
                .execute(&pool)
                .await;
        });

        let err = app
            .state
            .items()
            .update(
                item_id,
                UpdateItem {
                    title: None,
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: Some(json!({ "f_layout": invalid_page() })),
                    log: None,
                    stage_id: None,
                    author_id: None,
                    created: None,
                },
                &admin(uid),
            )
            .await
            .expect_err("an unknown component is refused on update");
        assert!(unknown_component_error(&err), "{err:#}");
    });
}

#[test]
fn import_refuses_an_unknown_component() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;

        let uid = create_user(app).await;

        let item_id = Uuid::now_v7();
        let line = json!({
            "type": "item",
            "id": item_id,
            "item_type": TYPE,
            "title": "K11 import",
            "author_id": uid,
            "created": 1_420_070_400,
            "changed": 1_420_070_400,
            "fields": { "f_layout": invalid_page() },
        });
        let err = import(&app.state, format!("{line}\n").as_bytes(), false)
            .await
            .expect_err("an unknown component is refused on import");
        let failed = err
            .downcast_ref::<ImportFailed>()
            .unwrap_or_else(|| panic!("not an ImportFailed: {err:#}"));
        assert!(
            failed.message.contains("unknown component type"),
            "{failed}"
        );

        // Nothing was written: the whole import rolled back on this line.
        let found: Option<i32> = sqlx::query_scalar("SELECT 1 FROM item WHERE id = $1")
            .bind(item_id)
            .fetch_optional(&app.db)
            .await
            .unwrap();
        assert!(found.is_none(), "the refused item must not exist");
    });
}
