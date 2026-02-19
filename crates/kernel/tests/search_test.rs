#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Search service tests.
//!
//! Tests for Phase 6A search functionality.

use uuid::Uuid;

// Unit tests that don't require database

#[test]
fn test_search_results_serde() {
    use trovato_kernel::search::{SearchResult, SearchResults};

    let results = SearchResults {
        query: "test query".to_string(),
        results: vec![
            SearchResult {
                id: Uuid::now_v7(),
                item_type: "page".to_string(),
                title: "Test Page".to_string(),
                rank: 0.5,
                snippet: Some("<mark>test</mark> content here".to_string()),
            },
            SearchResult {
                id: Uuid::now_v7(),
                item_type: "blog".to_string(),
                title: "Blog Post".to_string(),
                rank: 0.3,
                snippet: None,
            },
        ],
        total: 2,
        offset: 0,
        limit: 10,
    };

    let json = serde_json::to_string(&results).unwrap();
    let parsed: SearchResults = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.query, "test query");
    assert_eq!(parsed.total, 2);
    assert_eq!(parsed.results.len(), 2);
    assert_eq!(parsed.results[0].title, "Test Page");
    assert_eq!(parsed.results[1].title, "Blog Post");
}

#[test]
fn test_search_result_fields() {
    use trovato_kernel::search::SearchResult;

    let id = Uuid::now_v7();
    let result = SearchResult {
        id,
        item_type: "article".to_string(),
        title: "My Article".to_string(),
        rank: 0.75,
        snippet: Some("Article content".to_string()),
    };

    assert_eq!(result.id, id);
    assert_eq!(result.item_type, "article");
    assert_eq!(result.title, "My Article");
    assert!(result.rank > 0.5);
    assert!(result.snippet.is_some());
}

#[test]
fn test_field_config_serde() {
    use trovato_kernel::search::FieldConfig;

    let config = FieldConfig {
        field_name: "body".to_string(),
        weight: 'B',
    };

    let json = serde_json::to_string(&config).unwrap();
    assert!(json.contains("body"));
    assert!(json.contains("B"));

    let parsed: FieldConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.field_name, "body");
    assert_eq!(parsed.weight, 'B');
}

#[test]
fn test_empty_search_results() {
    use trovato_kernel::search::SearchResults;

    let results = SearchResults {
        query: "".to_string(),
        results: vec![],
        total: 0,
        offset: 0,
        limit: 10,
    };

    assert!(results.results.is_empty());
    assert_eq!(results.total, 0);
}

#[test]
fn test_pagination_values() {
    use trovato_kernel::search::SearchResults;

    let results = SearchResults {
        query: "test".to_string(),
        results: vec![],
        total: 100,
        offset: 20,
        limit: 10,
    };

    // Page 3 (offset 20, limit 10)
    let page = (results.offset / results.limit) + 1;
    let total_pages = (results.total + results.limit - 1) / results.limit;

    assert_eq!(page, 3);
    assert_eq!(total_pages, 10);
}
