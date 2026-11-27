#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A Link field's `title` and a List field's label are searchable text, on
//! both backends, exactly like a text field; a Link's `uri` and a List's bare
//! key are not (B302).
//!
//! `item_search_vector` (Postgres) and the Pagefind export used to read only
//! `Text`/`TextLong` field names out of `item_type.settings`, so an item
//! findable only by a link's title or a chosen list value was invisible to
//! search.

mod common;

use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::CreateItem;
use trovato_kernel::tap::UserContext;
use trovato_sdk::types::{FieldDefinition, FieldType};
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// A fresh type with a Link and a List field.
async fn fresh_type(app: &TestApp) -> String {
    let name = format!("b302s_{}", &Uuid::now_v7().simple().to_string()[20..]);
    let fields = vec![
        FieldDefinition::new("field_link", FieldType::Link).label("Link"),
        FieldDefinition::new("field_choice", FieldType::List)
            .label("Choice")
            .allowed_values(&[("xk1", "Golden Ticket"), ("xk2", "Silver Medal")]),
    ];
    app.state
        .content_types()
        .create(
            &name,
            "Search probe",
            Some("search link/list fixture"),
            serde_json::json!({ "fields": serde_json::to_value(&fields).unwrap() }),
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

/// What each backend finds for `word`, as the anonymous visitor.
async fn found(app: &TestApp, word: &str) -> (Vec<Uuid>, Vec<Uuid>) {
    let docs = trovato_kernel::cron::indexable_documents(&app.db, app.state.items())
        .await
        .expect("export");
    let pagefind = docs
        .iter()
        .filter(|d| trovato_kernel::cron::render_fragment(d).contains(word))
        .map(|d| d.id)
        .collect();
    let postgres = app
        .state
        .search()
        .search_unrecorded(word, &[LIVE_STAGE_ID], None, 50, 0)
        .await
        .expect("search")
        .results
        .into_iter()
        .map(|r| r.id)
        .collect();
    (pagefind, postgres)
}

#[test]
fn a_link_title_and_a_list_label_are_found_by_both_backends() {
    run_test(async {
        let app = shared_app().await;
        let item_type = fresh_type(app).await;
        let tag = Uuid::now_v7().simple().to_string();
        let title_word = format!("zqswlinktitle{tag}");
        let uri_word = format!("zqswlinkuri{tag}");

        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: item_type.clone(),
                    title: "B302 search probe".to_string(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: None,
                    sticky: None,
                    fields: Some(serde_json::json!({
                        "field_link": {
                            "uri": format!("/{uri_word}"),
                            "title": title_word,
                        },
                        "field_choice": "xk1",
                    })),
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &admin(),
            )
            .await
            .expect("create item");

        // The link's title, and the list's label, are findable.
        assert_eq!(
            found(app, &title_word).await,
            (vec![item.id], vec![item.id]),
            "a link's title is indexed"
        );
        assert_eq!(
            found(app, "Golden Ticket").await,
            (vec![item.id], vec![item.id]),
            "a list field's label is indexed"
        );

        // Neither backend finds the item by the link's uri or the list's
        // bare key: those are not prose a reader would search for.
        assert_eq!(
            found(app, &uri_word).await,
            (vec![], vec![]),
            "a link's uri is not indexed"
        );
        assert_eq!(
            found(app, "xk1").await,
            (vec![], vec![]),
            "a list field's bare key is not indexed"
        );

        let pool = app.db.clone();
        let id = item.id;
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
    });
}
