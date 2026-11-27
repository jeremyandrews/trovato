#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `trovato import` runs the same write check as every other path (B302):
//! an NDJSON item holding a Link, a List and a Telephone value imports, a
//! `javascript:` link or an unknown list key is refused, named with the
//! line it is on, and a Telephone with a country prefix gets its `e164`
//! filled in on the way in, as `ItemService` fills it in on every other
//! write path.

mod common;

use common::{TestApp, run_test, shared_app};
use serde_json::json;
use trovato_kernel::import::{ImportFailed, import};
use trovato_sdk::types::{FieldDefinition, FieldType};
use uuid::Uuid;

/// A content type with one Link, one single and one multi value List, and
/// one Telephone field, for this test's imports only.
async fn fresh_type(app: &TestApp) -> String {
    let name = format!("b302import_{}", &Uuid::now_v7().simple().to_string()[20..]);
    let fields = vec![
        FieldDefinition::new("field_link", FieldType::Link).label("Link"),
        FieldDefinition::new("field_choice", FieldType::List)
            .label("Choice")
            .allowed_values(&[("gold", "Gold Medal"), ("silver", "Silver Medal")]),
        FieldDefinition::new("field_tel", FieldType::Telephone).label("Telephone"),
    ];
    app.state
        .content_types()
        .create(
            &name,
            "B302 import fixture",
            Some("link/list/telephone import fixture"),
            json!({ "fields": serde_json::to_value(&fields).unwrap() }),
        )
        .await
        .expect("create the fixture type");

    let pool = app.db.clone();
    let registry = std::sync::Arc::clone(app.state.content_types());
    let machine_name = name.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE type = $1")
            .bind(&machine_name)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item_type WHERE type = $1")
            .bind(&machine_name)
            .execute(&pool)
            .await;
        registry.invalidate(&machine_name);
    });
    name
}

/// A user to author imported content, removed with what it wrote.
async fn author(app: &TestApp, prefix: &str) -> Uuid {
    let name = common::username(prefix);
    app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let id = common::user_id_of(app, &name).await;
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

fn forget_item(app: &TestApp, id: Uuid) {
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("UPDATE item SET current_revision_id = NULL WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item_revision WHERE item_id = $1")
            .bind(id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
}

fn item_line(id: Uuid, item_type: &str, author: Uuid, title: &str, fields: serde_json::Value) -> serde_json::Value {
    json!({
        "type": "item", "id": id, "item_type": item_type, "title": title,
        "author_id": author, "created": 1_767_225_600i64, "changed": 1_767_225_600i64,
        "fields": fields,
    })
}

async fn stored_fields(app: &TestApp, id: Uuid) -> serde_json::Value {
    sqlx::query_scalar("SELECT fields FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

#[test]
fn an_ndjson_item_with_all_three_kinds_imports() {
    run_test(async {
        let app = shared_app().await;
        let item_type = fresh_type(app).await;
        let writer = author(app, "b302_import_author").await;
        let id = Uuid::now_v7();
        forget_item(app, id);

        let line = item_line(
            id,
            &item_type,
            writer,
            "B302 import good",
            json!({
                "field_link": {"uri": "https://example.org/about", "title": "About", "new_tab": true},
                "field_choice": "gold",
                "field_tel": "+39 0583 123456",
            }),
        );
        let text = format!("{line}\n");
        let report = import(&app.state, text.as_bytes(), false)
            .await
            .expect("all three kinds import");
        assert_eq!(report.of("item").created, 1);

        let fields = stored_fields(app, id).await;
        assert_eq!(
            fields["field_link"],
            json!({"uri": "https://example.org/about", "title": "About", "new_tab": true})
        );
        assert_eq!(fields["field_choice"], json!("gold"));
        assert_eq!(
            fields["field_tel"],
            json!({"value": "+39 0583 123456", "e164": "+390583123456"}),
            "e164 is filled in on import, as on every other write path"
        );
    });
}

#[test]
fn a_javascript_link_is_refused_with_its_line_number() {
    run_test(async {
        let app = shared_app().await;
        let item_type = fresh_type(app).await;
        let writer = author(app, "b302_import_author").await;
        let good_id = Uuid::now_v7();
        let bad_id = Uuid::now_v7();
        forget_item(app, good_id);
        forget_item(app, bad_id);

        let good = item_line(good_id, &item_type, writer, "B302 import good", json!({}));
        let bad = item_line(
            bad_id,
            &item_type,
            writer,
            "B302 import bad link",
            json!({"field_link": {"uri": "javascript:alert(1)"}}),
        );
        let text = format!("{good}\n{bad}\n");
        let error = import(&app.state, text.as_bytes(), false)
            .await
            .expect_err("a javascript: link is refused");
        let failed = error
            .downcast_ref::<ImportFailed>()
            .expect("an ImportFailed");
        assert_eq!(failed.line, 2, "{failed}");
        assert!(format!("{failed}").contains("Link"), "{failed}");

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM item WHERE id = ANY($1)")
            .bind([good_id, bad_id].as_slice())
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(count, 0, "nothing was written, the good line included");
    });
}

#[test]
fn an_unknown_list_key_is_refused_with_its_line_number() {
    run_test(async {
        let app = shared_app().await;
        let item_type = fresh_type(app).await;
        let writer = author(app, "b302_import_author").await;
        let good_id = Uuid::now_v7();
        let bad_id = Uuid::now_v7();
        forget_item(app, good_id);
        forget_item(app, bad_id);

        let good = item_line(good_id, &item_type, writer, "B302 import good", json!({}));
        let bad = item_line(
            bad_id,
            &item_type,
            writer,
            "B302 import bad list",
            json!({"field_choice": "platinum"}),
        );
        let text = format!("{good}\n{bad}\n");
        let error = import(&app.state, text.as_bytes(), false)
            .await
            .expect_err("an unknown list key is refused");
        let failed = error
            .downcast_ref::<ImportFailed>()
            .expect("an ImportFailed");
        assert_eq!(failed.line, 2, "{failed}");
        assert!(format!("{failed}").contains("platinum"), "{failed}");

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM item WHERE id = ANY($1)")
            .bind([good_id, bad_id].as_slice())
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(count, 0, "nothing was written, the good line included");
    });
}
