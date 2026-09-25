#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A gather query in `table` format renders a table.
//!
//! `templates/gather/query--table.html` iterates `columns`, and no kernel code
//! ever put `columns` in the render context. Every query in `table` format —
//! 23 of 29 in this checkout's database, including nine core administrative
//! listings — therefore failed to render, logged the failure, and fell through
//! to a fallback that dumped whatever keys the rows happened to carry. The page
//! answered 200 throughout, so nothing upstream noticed.
//!
//! Two shapes have to work, because the core listings are the second one:
//!
//! - a query that names its fields, which renders them in declared order under
//!   their labels;
//! - a query that names none, which is a `SELECT base.*`, and can only take its
//!   columns from the rows.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};

use trovato_kernel::gather::{
    DisplayFormat, FilterOperator, FilterValue, GatherQuery, PagerConfig, PagerStyle,
    QueryDefinition, QueryDisplay, QueryField, QueryFilter,
};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Query ids are per test rather than per file: both cases register a query
/// filtered to their own seeded item, and this binary runs its tests in
/// parallel against one shared gather registry, so a shared id means whichever
/// test registers second decides what the other one renders.
const DECLARED: &str = "table_fmt_declared";
const SELECT_STAR: &str = "table_fmt_select_star";

fn table_display() -> QueryDisplay {
    QueryDisplay {
        format: DisplayFormat::Table,
        items_per_page: 10,
        pager: PagerConfig {
            enabled: false,
            style: PagerStyle::Full,
            show_count: false,
        },
        empty_text: None,
        header: None,
        footer: None,
        canonical_url: None,
        routes: Vec::new(),
        feed: None,
    }
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// The one conference this file asserts on, and the city it carries.
const CITY: &str = "Barga";

/// Seed one conference with a unique title, and register the two probe queries
/// filtered down to it.
///
/// A title filter rather than the definition's `item_type`, so the assertions
/// are about rendering and not about which rows the engine selected.
async fn ensure_queries(app: &TestApp, declared_id: &str, star_id: &str) -> String {
    app.ensure_conference_type().await;

    let title = format!("TableFmt Conf {}", Uuid::now_v7().simple());
    app.state
        .items()
        .create(
            CreateItem {
                item_type: "conference".to_string(),
                title: title.clone(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({ "field_city": { "value": CITY } })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("gather table format test".to_string()),
            },
            &UserContext::background(),
        )
        .await
        .expect("create the probe conference");

    let only_this_item = || QueryFilter {
        field: "title".to_string(),
        operator: FilterOperator::Equals,
        value: FilterValue::String(title.clone()),
        exposed: false,
        exposed_label: None,
        widget: Default::default(),
    };

    app.state
        .gather()
        .register_query(GatherQuery {
            query_id: declared_id.to_string(),
            label: "Declared Columns".to_string(),
            description: None,
            definition: QueryDefinition {
                base_table: "item".to_string(),
                fields: vec![
                    QueryField {
                        field_name: "title".to_string(),
                        table_alias: None,
                        label: Some("Conference".to_string()),
                    },
                    QueryField {
                        field_name: "fields.field_city".to_string(),
                        table_alias: None,
                        label: Some("City".to_string()),
                    },
                ],
                filters: vec![only_this_item()],
                ..Default::default()
            },
            display: table_display(),
            plugin: "core".to_string(),
            created: 0,
            changed: 0,
        })
        .await
        .expect("register the declared-fields query");

    app.state
        .gather()
        .register_query(GatherQuery {
            query_id: star_id.to_string(),
            label: "Select Star".to_string(),
            description: None,
            definition: QueryDefinition {
                base_table: "item".to_string(),
                // No fields: the shape every core administrative listing has.
                filters: vec![only_this_item()],
                ..Default::default()
            },
            display: table_display(),
            plugin: "core".to_string(),
            created: 0,
            changed: 0,
        })
        .await
        .expect("register the select-star query");

    title
}

async fn render(app: &TestApp, query_id: &str) -> String {
    let response = app
        .request(
            Request::builder()
                .uri(format!("/gather/{query_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    body_string(response).await
}

/// Count the `<th>` cells in the first table of a page.
fn header_cells(html: &str) -> Vec<String> {
    html.match_indices("<th>")
        .filter_map(|(at, _)| {
            let rest = &html[at + 4..];
            rest.find("</th>").map(|end| rest[..end].trim().to_string())
        })
        .collect()
}

/// **The finding, closed.** A `table` query renders through its own template
/// rather than failing and falling back.
#[test]
fn a_table_query_with_declared_fields_renders_its_columns() {
    run_test(async {
        let app = shared_app().await;
        let declared_id = format!("{DECLARED}_a");
        let star_id = format!("{SELECT_STAR}_a");
        let title = ensure_queries(app, &declared_id, &star_id).await;

        let html = render(app, &declared_id).await;

        assert!(
            html.contains("gather-table"),
            "the table template must be the one that rendered; the fallback markup has no \
             gather-table class, which is how this defect stayed invisible behind a 200"
        );

        let headers = header_cells(&html);
        assert_eq!(
            headers,
            vec!["Conference".to_string(), "City".to_string()],
            "declared fields must render in declared order under their labels"
        );

        assert!(
            html.contains(&title),
            "the declared 'title' column must render its value; a labelled plain column used \
             to be dropped from the row entirely"
        );
        assert!(
            html.contains(CITY),
            "the declared JSONB column must render its value too"
        );
    });
}

/// The shape every core administrative listing has: no declared fields at all.
#[test]
fn a_table_query_selecting_everything_still_renders_a_table() {
    run_test(async {
        let app = shared_app().await;
        let declared_id = format!("{DECLARED}_b");
        let star_id = format!("{SELECT_STAR}_b");
        let title = ensure_queries(app, &declared_id, &star_id).await;

        let html = render(app, &star_id).await;

        assert!(
            html.contains("gather-table"),
            "a SELECT * query in table format must render through the table template"
        );

        let headers = header_cells(&html);
        assert!(
            headers.iter().any(|h| h == "title"),
            "columns must be derived from the rows when the query declares none; got {headers:?}"
        );
        assert!(
            html.contains(&title),
            "the rendered table must carry its rows' values"
        );
    });
}
