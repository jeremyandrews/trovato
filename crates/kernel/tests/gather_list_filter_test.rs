#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A `List` field is filterable in Gather (B302): a single value List filter
//! compares with `equals` on the key, a multi value List filter matches when
//! its stored array contains the key (the built-in `jsonb_array_contains`
//! condition, reached from a query definition with no plugin involved), and
//! an exposed filter on a List field, left at its default `Text` widget,
//! renders as a select offering the field's allowed values (labels shown,
//! keys submitted) rather than a free text box.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, defer_cleanup, run_test, shared_app};
use std::collections::HashMap;
use trovato_kernel::gather::{
    DisplayFormat, FilterOperator, FilterValue, PagerConfig, PagerStyle, QueryContext,
    QueryDefinition, QueryDisplay, QueryFilter,
};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use trovato_sdk::types::{FieldDefinition, FieldType};
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

fn admin_ctx() -> QueryContext {
    QueryContext {
        viewer: Some(admin()),
        ..QueryContext::default()
    }
}

/// A fresh content type with a single value and a multi value List field,
/// both sharing the same allowed values.
async fn fresh_type(app: &TestApp) -> String {
    let name = format!("b302g_{}", &Uuid::now_v7().simple().to_string()[20..]);
    let fields = vec![
        FieldDefinition::new("field_choice", FieldType::List)
            .label("Choice")
            .allowed_values(&[("gold", "Gold Medal"), ("silver", "Silver Medal")]),
        FieldDefinition::new("field_choices", FieldType::List)
            .label("Choices")
            .cardinality(-1)
            .allowed_values(&[("gold", "Gold Medal"), ("silver", "Silver Medal")]),
    ];
    app.state
        .content_types()
        .create(
            &name,
            "B302 gather fixture",
            Some("link/list/telephone gather fixture"),
            serde_json::json!({ "fields": serde_json::to_value(&fields).unwrap() }),
        )
        .await
        .expect("create the fixture type");

    let pool = app.db.clone();
    let registry = std::sync::Arc::clone(app.state.content_types());
    let machine_name = name.clone();
    defer_cleanup(move || async move {
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

async fn published(app: &TestApp, item_type: &str, title: &str, fields: serde_json::Value) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: item_type.to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(fields),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: None,
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id;
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

fn test_display() -> QueryDisplay {
    QueryDisplay {
        format: DisplayFormat::Table,
        items_per_page: Some(50),
        pager: PagerConfig {
            enabled: true,
            style: PagerStyle::Full,
            show_count: true,
            zero_based: false,
        },
        empty_text: None,
        header: None,
        footer: None,
        canonical_url: None,
        routes: Vec::new(),
        feed: None,
    }
}

fn single_choice_definition(item_type: &str) -> QueryDefinition {
    QueryDefinition {
        base_table: "item".to_string(),
        item_type: Some(item_type.to_string()),
        filters: vec![QueryFilter {
            field: "fields.field_choice".to_string(),
            operator: FilterOperator::Equals,
            value: FilterValue::Null(()),
            exposed: true,
            exposed_label: Some("Choice".to_string()),
            widget: Default::default(),
        }],
        ..Default::default()
    }
}

fn multi_choices_definition(item_type: &str) -> QueryDefinition {
    QueryDefinition {
        base_table: "item".to_string(),
        item_type: Some(item_type.to_string()),
        filters: vec![QueryFilter {
            field: "fields.field_choices".to_string(),
            operator: FilterOperator::Custom("jsonb_array_contains".to_string()),
            value: FilterValue::Null(()),
            exposed: true,
            exposed_label: Some("Choices".to_string()),
            widget: Default::default(),
        }],
        ..Default::default()
    }
}

#[test]
fn a_single_list_filter_matches_by_key_not_label() {
    run_test(async {
        let app = shared_app().await;
        let item_type = fresh_type(app).await;
        published(
            app,
            &item_type,
            "B302 gold item",
            serde_json::json!({"field_choice": "gold"}),
        )
        .await;
        published(
            app,
            &item_type,
            "B302 silver item",
            serde_json::json!({"field_choice": "silver"}),
        )
        .await;

        let definition = single_choice_definition(&item_type);
        let mut exposed = HashMap::new();
        exposed.insert(
            "fields.field_choice".to_string(),
            FilterValue::String("gold".to_string()),
        );
        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &test_display(),
                1,
                exposed,
                LIVE_STAGE_ID,
                &admin_ctx(),
            )
            .await
            .expect("filter by key");
        assert_eq!(result.total, 1, "{:?}", result.items);
    });
}

#[test]
fn a_multi_list_filter_matches_when_the_array_contains_the_key() {
    run_test(async {
        let app = shared_app().await;
        let item_type = fresh_type(app).await;
        published(
            app,
            &item_type,
            "B302 both",
            serde_json::json!({"field_choices": ["gold", "silver"]}),
        )
        .await;
        published(
            app,
            &item_type,
            "B302 silver only",
            serde_json::json!({"field_choices": ["silver"]}),
        )
        .await;

        let definition = multi_choices_definition(&item_type);
        let mut exposed = HashMap::new();
        exposed.insert(
            "fields.field_choices".to_string(),
            FilterValue::String("gold".to_string()),
        );
        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &test_display(),
                1,
                exposed,
                LIVE_STAGE_ID,
                &admin_ctx(),
            )
            .await
            .expect("filter by containment");
        assert_eq!(result.total, 1, "{:?}", result.items);
    });
}

/// Registered through the real router: the exposed filter form on a List
/// field's default `Text` widget renders a select of the allowed values
/// (labels shown, keys submitted as option values), with "Any" first, and a
/// value selected in the query string narrows the listed rows.
#[test]
fn the_exposed_filter_form_offers_allowed_values_and_narrows_results() {
    run_test(async {
        let app = shared_app().await;
        let item_type = fresh_type(app).await;
        published(
            app,
            &item_type,
            "B302 gold row",
            serde_json::json!({"field_choice": "gold"}),
        )
        .await;
        published(
            app,
            &item_type,
            "B302 silver row",
            serde_json::json!({"field_choice": "silver"}),
        )
        .await;

        let query_id = format!("b302_list_filter_{}", Uuid::now_v7().simple());
        let query: trovato_kernel::gather::GatherQuery =
            serde_json::from_value(serde_json::json!({
                "query_id": query_id,
                "label": "B302 list filter",
                "definition": {
                    "base_table": "item",
                    "item_type": item_type,
                    "filters": [{
                        "exposed": true,
                        "field": "fields.field_choice",
                        "operator": "equals",
                        "value": null,
                        "exposed_label": "Choice",
                    }],
                    "sorts": [{ "field": "title", "direction": "asc", "nulls": null }],
                    "stage_aware": true,
                },
                "display": {
                    "format": "table",
                    "items_per_page": 20,
                    "canonical_url": format!("/b302-list-filter-{query_id}"),
                },
                "plugin": "core"
            }))
            .expect("gather deserializes");
        app.state
            .gather()
            .register_query(query)
            .await
            .expect("register gather");
        let state = app.state.clone();
        let cleanup_id = query_id.clone();
        defer_cleanup(move || async move {
            let _ = state.gather().delete_query(&cleanup_id).await;
        });

        let name = common::username("b302list");
        let cookies = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple",
                &format!("{name}@test.local"),
            )
            .await;

        let plain = app
            .request_with_cookies(
                Request::builder()
                    .uri(format!("/gather/{query_id}"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(plain.status(), StatusCode::OK);
        let html = String::from_utf8_lossy(
            &axum::body::to_bytes(plain.into_body(), 4 * 1024 * 1024)
                .await
                .unwrap(),
        )
        .to_string();
        assert!(
            html.contains(r#"<option value="">Any</option>"#),
            "an unselected choice: {html}"
        );
        assert!(
            html.contains(r#"<option value="gold">Gold Medal</option>"#),
            "the label is shown, the key is the option's value: {html}"
        );
        assert!(
            html.contains(r#"<option value="silver">Silver Medal</option>"#),
            "{html}"
        );
        assert!(html.contains("B302 gold row"), "{html}");
        assert!(html.contains("B302 silver row"), "{html}");

        let filtered = app
            .request_with_cookies(
                Request::builder()
                    .uri(format!("/gather/{query_id}?fields.field_choice=gold"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(filtered.status(), StatusCode::OK);
        let html = String::from_utf8_lossy(
            &axum::body::to_bytes(filtered.into_body(), 4 * 1024 * 1024)
                .await
                .unwrap(),
        )
        .to_string();
        assert!(html.contains("B302 gold row"), "{html}");
        assert!(
            !html.contains("B302 silver row"),
            "narrowed to gold: {html}"
        );
    });
}
