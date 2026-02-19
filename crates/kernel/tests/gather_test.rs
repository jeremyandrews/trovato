#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Gather query engine integration tests.
//!
//! Tests for QueryDefinition, QueryDisplay, query building, and results.

use trovato_kernel::gather::{
    DisplayFormat, FilterOperator, FilterValue, GatherQuery, GatherResult, NullsOrder, PagerConfig,
    PagerStyle, QueryDefinition, QueryDisplay, QueryField, QueryFilter, QueryRelationship,
    QuerySort, SortDirection,
};
use uuid::Uuid;

// -------------------------------------------------------------------------
// QueryDefinition tests
// -------------------------------------------------------------------------

#[test]
fn query_definition_defaults() {
    let def = QueryDefinition::default();

    assert_eq!(def.base_table, "item");
    assert!(def.item_type.is_none());
    assert!(def.fields.is_empty());
    assert!(def.filters.is_empty());
    assert!(def.sorts.is_empty());
    assert!(def.relationships.is_empty());
}

#[test]
fn query_definition_with_filters() {
    let def = QueryDefinition {
        base_table: "item".to_string(),
        item_type: Some("blog".to_string()),
        filters: vec![
            QueryFilter {
                field: "status".to_string(),
                operator: FilterOperator::Equals,
                value: FilterValue::Integer(1),
                exposed: false,
                exposed_label: None,
            },
            QueryFilter {
                field: "title".to_string(),
                operator: FilterOperator::Contains,
                value: FilterValue::String("rust".to_string()),
                exposed: true,
                exposed_label: Some("Search".to_string()),
            },
        ],
        ..Default::default()
    };

    assert_eq!(def.filters.len(), 2);
    assert_eq!(def.filters[0].operator, FilterOperator::Equals);
    assert!(def.filters[1].exposed);
}

#[test]
fn query_definition_with_sorts() {
    let def = QueryDefinition {
        base_table: "item".to_string(),
        sorts: vec![
            QuerySort {
                field: "sticky".to_string(),
                direction: SortDirection::Desc,
                nulls: None,
            },
            QuerySort {
                field: "created".to_string(),
                direction: SortDirection::Desc,
                nulls: Some(NullsOrder::Last),
            },
        ],
        ..Default::default()
    };

    assert_eq!(def.sorts.len(), 2);
    assert_eq!(def.sorts[0].direction, SortDirection::Desc);
    assert_eq!(def.sorts[1].nulls, Some(NullsOrder::Last));
}

#[test]
fn query_definition_serialization() {
    let def = QueryDefinition {
        base_table: "item".to_string(),
        item_type: Some("page".to_string()),
        stage_aware: true,
        fields: vec![QueryField {
            field_name: "title".to_string(),
            table_alias: None,
            label: Some("Title".to_string()),
        }],
        filters: vec![QueryFilter {
            field: "status".to_string(),
            operator: FilterOperator::Equals,
            value: FilterValue::Integer(1),
            exposed: false,
            exposed_label: None,
        }],
        sorts: vec![QuerySort {
            field: "created".to_string(),
            direction: SortDirection::Desc,
            nulls: None,
        }],
        relationships: vec![],
        includes: std::collections::HashMap::new(),
    };

    let json = serde_json::to_string(&def).unwrap();
    let parsed: QueryDefinition = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.item_type, Some("page".to_string()));
    assert_eq!(parsed.fields.len(), 1);
}

// -------------------------------------------------------------------------
// FilterOperator tests
// -------------------------------------------------------------------------

#[test]
fn filter_operators() {
    // Comparison operators
    assert_eq!(
        serde_json::to_string(&FilterOperator::Equals).unwrap(),
        "\"equals\""
    );
    assert_eq!(
        serde_json::to_string(&FilterOperator::NotEquals).unwrap(),
        "\"not_equals\""
    );
    assert_eq!(
        serde_json::to_string(&FilterOperator::GreaterThan).unwrap(),
        "\"greater_than\""
    );

    // String operators
    assert_eq!(
        serde_json::to_string(&FilterOperator::Contains).unwrap(),
        "\"contains\""
    );
    assert_eq!(
        serde_json::to_string(&FilterOperator::StartsWith).unwrap(),
        "\"starts_with\""
    );

    // Null operators
    assert_eq!(
        serde_json::to_string(&FilterOperator::IsNull).unwrap(),
        "\"is_null\""
    );
    assert_eq!(
        serde_json::to_string(&FilterOperator::IsNotNull).unwrap(),
        "\"is_not_null\""
    );

    // Category operators
    assert_eq!(
        serde_json::to_string(&FilterOperator::HasTag).unwrap(),
        "\"has_tag\""
    );
    assert_eq!(
        serde_json::to_string(&FilterOperator::HasTagOrDescendants).unwrap(),
        "\"has_tag_or_descendants\""
    );
}

#[test]
fn filter_operator_deserialization() {
    let parsed: FilterOperator = serde_json::from_str("\"has_tag_or_descendants\"").unwrap();
    assert_eq!(parsed, FilterOperator::HasTagOrDescendants);

    let parsed: FilterOperator = serde_json::from_str("\"equals\"").unwrap();
    assert_eq!(parsed, FilterOperator::Equals);
}

// -------------------------------------------------------------------------
// FilterValue tests
// -------------------------------------------------------------------------

#[test]
fn filter_value_types() {
    // String
    let str_val = FilterValue::String("test".to_string());
    assert_eq!(str_val.as_string(), Some("test".to_string()));

    // Integer
    let int_val = FilterValue::Integer(42);
    assert_eq!(int_val.as_i64(), Some(42));

    // Float
    #[allow(clippy::approx_constant)] // intentionally testing with approximate pi
    let float_val = FilterValue::Float(3.14);
    assert!(float_val.as_i64().is_none());

    // Boolean
    let bool_val = FilterValue::Boolean(true);
    assert_eq!(bool_val.as_string(), Some("true".to_string()));

    // UUID
    let uuid = Uuid::nil();
    let uuid_val = FilterValue::Uuid(uuid);
    assert_eq!(uuid_val.as_uuid(), Some(uuid));
}

#[test]
fn filter_value_uuid_list() {
    let uuid1 = Uuid::nil();
    let uuid2 = Uuid::now_v7();

    // Single UUID
    let single = FilterValue::Uuid(uuid1);
    let list = single.as_uuid_list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0], uuid1);

    // List of UUIDs
    let multi = FilterValue::List(vec![FilterValue::Uuid(uuid1), FilterValue::Uuid(uuid2)]);
    let list = multi.as_uuid_list();
    assert_eq!(list.len(), 2);
}

#[test]
fn filter_value_serialization() {
    let val = FilterValue::Integer(100);
    let json = serde_json::to_string(&val).unwrap();
    assert_eq!(json, "100");

    let val = FilterValue::String("hello".to_string());
    let json = serde_json::to_string(&val).unwrap();
    assert_eq!(json, "\"hello\"");

    let uuid = Uuid::nil();
    let val = FilterValue::Uuid(uuid);
    let json = serde_json::to_string(&val).unwrap();
    assert!(json.contains("00000000-0000-0000-0000-000000000000"));
}

// -------------------------------------------------------------------------
// QueryDisplay tests
// -------------------------------------------------------------------------

#[test]
fn query_display_defaults() {
    let display = QueryDisplay::default();

    assert_eq!(display.items_per_page, 10);
    assert_eq!(display.format, DisplayFormat::Table);
    assert!(display.pager.enabled);
    assert!(display.pager.show_count);
}

#[test]
fn query_display_custom() {
    let display = QueryDisplay {
        format: DisplayFormat::Grid,
        items_per_page: 20,
        pager: PagerConfig {
            enabled: true,
            style: PagerStyle::Mini,
            show_count: false,
        },
        empty_text: Some("No items found".to_string()),
        header: Some("Results".to_string()),
        footer: None,
    };

    assert_eq!(display.format, DisplayFormat::Grid);
    assert_eq!(display.items_per_page, 20);
    assert_eq!(display.pager.style, PagerStyle::Mini);
    assert!(!display.pager.show_count);
}

#[test]
fn display_format_variants() {
    assert_eq!(DisplayFormat::Table, DisplayFormat::Table);
    assert_eq!(DisplayFormat::List, DisplayFormat::List);
    assert_eq!(DisplayFormat::Grid, DisplayFormat::Grid);

    let custom = DisplayFormat::Custom("my_template".to_string());
    if let DisplayFormat::Custom(name) = custom {
        assert_eq!(name, "my_template");
    } else {
        panic!("Expected Custom variant");
    }
}

#[test]
fn pager_styles() {
    assert_eq!(PagerStyle::Full, PagerStyle::Full);
    assert_eq!(PagerStyle::Mini, PagerStyle::Mini);
    assert_eq!(PagerStyle::Infinite, PagerStyle::Infinite);

    // Default is Full
    let config = PagerConfig::default();
    assert_eq!(config.style, PagerStyle::Full);
}

// -------------------------------------------------------------------------
// GatherResult tests
// -------------------------------------------------------------------------

#[test]
fn gather_result_creation() {
    let items = vec![
        serde_json::json!({"id": 1, "title": "Post 1"}),
        serde_json::json!({"id": 2, "title": "Post 2"}),
    ];

    let result = GatherResult::new(items.clone(), 100, 1, 10);

    assert_eq!(result.items.len(), 2);
    assert_eq!(result.total, 100);
    assert_eq!(result.page, 1);
    assert_eq!(result.per_page, 10);
    assert_eq!(result.total_pages, 10);
    assert!(!result.has_prev);
    assert!(result.has_next);
}

#[test]
fn gather_result_middle_page() {
    let result = GatherResult::new(vec![], 100, 5, 10);

    assert_eq!(result.page, 5);
    assert_eq!(result.total_pages, 10);
    assert!(result.has_prev);
    assert!(result.has_next);
}

#[test]
fn gather_result_last_page() {
    let result = GatherResult::new(vec![], 100, 10, 10);

    assert_eq!(result.page, 10);
    assert!(result.has_prev);
    assert!(!result.has_next);
}

#[test]
fn gather_result_single_page() {
    let result = GatherResult::new(vec![], 5, 1, 10);

    assert_eq!(result.total_pages, 1);
    assert!(!result.has_prev);
    assert!(!result.has_next);
}

#[test]
fn gather_result_empty() {
    let result = GatherResult::empty(1, 10);

    assert!(result.items.is_empty());
    assert_eq!(result.total, 0);
    assert_eq!(result.total_pages, 0);
    assert!(!result.has_prev);
    assert!(!result.has_next);
}

#[test]
fn gather_result_serialization() {
    let result = GatherResult::new(vec![serde_json::json!({"title": "Test"})], 1, 1, 10);

    let json = serde_json::to_string(&result).unwrap();
    assert!(json.contains("\"total\":1"));
    assert!(json.contains("\"has_next\":false"));
}

// -------------------------------------------------------------------------
// GatherQuery tests
// -------------------------------------------------------------------------

#[test]
fn gather_query_creation() {
    let gq = GatherQuery {
        query_id: "recent_articles".to_string(),
        label: "Recent Articles".to_string(),
        description: Some("Shows the most recent blog posts".to_string()),
        definition: QueryDefinition {
            base_table: "item".to_string(),
            item_type: Some("blog".to_string()),
            sorts: vec![QuerySort {
                field: "created".to_string(),
                direction: SortDirection::Desc,
                nulls: None,
            }],
            ..Default::default()
        },
        display: QueryDisplay {
            items_per_page: 10,
            ..Default::default()
        },
        plugin: "blog".to_string(),
        created: 1000,
        changed: 1000,
    };

    assert_eq!(gq.query_id, "recent_articles");
    assert_eq!(gq.plugin, "blog");
    assert_eq!(gq.definition.item_type, Some("blog".to_string()));
}

#[test]
fn gather_query_serialization() {
    let gq = GatherQuery {
        query_id: "test_query".to_string(),
        label: "Test Query".to_string(),
        description: None,
        definition: QueryDefinition::default(),
        display: QueryDisplay::default(),
        plugin: "core".to_string(),
        created: 1000,
        changed: 2000,
    };

    let json = serde_json::to_string(&gq).unwrap();
    let parsed: GatherQuery = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.query_id, "test_query");
    assert_eq!(parsed.created, 1000);
    assert_eq!(parsed.changed, 2000);
}

// -------------------------------------------------------------------------
// QueryRelationship tests
// -------------------------------------------------------------------------

#[test]
fn query_relationship() {
    use trovato_kernel::gather::JoinType;

    let rel = QueryRelationship {
        name: "author".to_string(),
        target_table: "users".to_string(),
        join_type: JoinType::Left,
        local_field: "author_id".to_string(),
        foreign_field: "id".to_string(),
    };

    assert_eq!(rel.name, "author");
    assert_eq!(rel.join_type, JoinType::Left);
}

// -------------------------------------------------------------------------
// Gate test: Recent Articles with Category Filter
// -------------------------------------------------------------------------

/// Gate test: Verify a "Recent Articles" query definition with category filter
/// can be constructed and serialized correctly.
#[test]
fn gate_test_recent_articles_query_definition() {
    // This is the gate test case from the Phase 4 requirements:
    // "Recent Articles" Gather query with category filter + pager

    let tech_tag_id = Uuid::nil(); // In real test, this would be a real tag ID

    let gq = GatherQuery {
        query_id: "recent_articles".to_string(),
        label: "Recent Articles".to_string(),
        description: Some("Blog posts filtered by category with pagination".to_string()),
        definition: QueryDefinition {
            base_table: "item".to_string(),
            item_type: Some("blog".to_string()),
            stage_aware: true,
            fields: vec![
                QueryField {
                    field_name: "id".to_string(),
                    table_alias: None,
                    label: None,
                },
                QueryField {
                    field_name: "title".to_string(),
                    table_alias: None,
                    label: Some("Title".to_string()),
                },
                QueryField {
                    field_name: "created".to_string(),
                    table_alias: None,
                    label: Some("Date".to_string()),
                },
                QueryField {
                    field_name: "fields.summary".to_string(),
                    table_alias: None,
                    label: Some("Summary".to_string()),
                },
            ],
            filters: vec![
                // Published only
                QueryFilter {
                    field: "status".to_string(),
                    operator: FilterOperator::Equals,
                    value: FilterValue::Integer(1),
                    exposed: false,
                    exposed_label: None,
                },
                // Category filter with hierarchy support
                QueryFilter {
                    field: "fields.category".to_string(),
                    operator: FilterOperator::HasTagOrDescendants,
                    value: FilterValue::Uuid(tech_tag_id),
                    exposed: true,
                    exposed_label: Some("Category".to_string()),
                },
            ],
            sorts: vec![
                // Sticky first, then by date
                QuerySort {
                    field: "sticky".to_string(),
                    direction: SortDirection::Desc,
                    nulls: None,
                },
                QuerySort {
                    field: "created".to_string(),
                    direction: SortDirection::Desc,
                    nulls: None,
                },
            ],
            relationships: vec![],
            includes: std::collections::HashMap::new(),
        },
        display: QueryDisplay {
            format: DisplayFormat::List,
            items_per_page: 10,
            pager: PagerConfig {
                enabled: true,
                style: PagerStyle::Full,
                show_count: true,
            },
            empty_text: Some("No articles found in this category.".to_string()),
            header: None,
            footer: None,
        },
        plugin: "blog".to_string(),
        created: chrono::Utc::now().timestamp(),
        changed: chrono::Utc::now().timestamp(),
    };

    // Verify serialization round-trip
    let json = serde_json::to_string_pretty(&gq).unwrap();
    let parsed: GatherQuery = serde_json::from_str(&json).unwrap();

    // Verify key properties
    assert_eq!(parsed.query_id, "recent_articles");
    assert_eq!(parsed.definition.item_type, Some("blog".to_string()));
    assert_eq!(parsed.definition.filters.len(), 2);
    assert_eq!(parsed.definition.sorts.len(), 2);
    assert_eq!(parsed.display.items_per_page, 10);
    assert!(parsed.display.pager.enabled);

    // Verify category filter
    let category_filter = &parsed.definition.filters[1];
    assert_eq!(category_filter.field, "fields.category");
    assert_eq!(
        category_filter.operator,
        FilterOperator::HasTagOrDescendants
    );
    assert!(category_filter.exposed);

    // Verify result structure would be correct
    let mock_result = GatherResult::new(
        vec![serde_json::json!({
            "id": "01234567-89ab-cdef-0123-456789abcdef",
            "title": "Getting Started with Rust",
            "created": 1707782400,
            "summary": "A beginner's guide to Rust programming"
        })],
        1,
        1,
        10,
    );

    assert_eq!(mock_result.total, 1);
    assert_eq!(mock_result.page, 1);
    assert!(!mock_result.has_next);
    assert!(!mock_result.has_prev);
}

// -------------------------------------------------------------------------
// JSONB field tests
// -------------------------------------------------------------------------

#[test]
fn jsonb_field_in_query_definition() {
    let def = QueryDefinition {
        base_table: "item".to_string(),
        fields: vec![
            QueryField {
                field_name: "fields.body".to_string(),
                table_alias: None,
                label: Some("Body".to_string()),
            },
            QueryField {
                field_name: "fields.tags".to_string(),
                table_alias: None,
                label: Some("Tags".to_string()),
            },
        ],
        filters: vec![QueryFilter {
            field: "fields.featured".to_string(),
            operator: FilterOperator::Equals,
            value: FilterValue::Boolean(true),
            exposed: false,
            exposed_label: None,
        }],
        ..Default::default()
    };

    assert_eq!(def.fields.len(), 2);
    assert!(def.fields[0].field_name.starts_with("fields."));
    assert!(def.filters[0].field.starts_with("fields."));
}
