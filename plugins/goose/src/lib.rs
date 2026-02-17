//! Goose plugin for Trovato.
//!
//! Load testing use case: 5 content types for test runs, scenarios,
//! endpoint results, sites, and comparisons. Validates high-throughput
//! write patterns.

use trovato_sdk::prelude::*;

/// The 5 Goose content types.
///
/// Uses `field_` prefix (new plugin, no existing data constraints).
#[plugin_tap]
pub fn tap_item_info() -> Vec<ContentTypeDefinition> {
    vec![
        ContentTypeDefinition {
            machine_name: "goose_test_run".into(),
            label: "Test Run".into(),
            description: "Load test execution with aggregate metrics".into(),
            fields: vec![
                FieldDefinition::new(
                    "field_target_site_id",
                    FieldType::RecordReference("goose_site".into()),
                )
                .required()
                .label("Target Site"),
                FieldDefinition::new("field_start_time", FieldType::Integer)
                    .required()
                    .label("Start Time"),
                FieldDefinition::new("field_end_time", FieldType::Integer).label("End Time"),
                FieldDefinition::new("field_config", FieldType::TextLong).label("Configuration"),
                FieldDefinition::new("field_status", FieldType::Text { max_length: None })
                    .label("Status"),
                FieldDefinition::new("field_aggregate_metrics", FieldType::TextLong)
                    .label("Aggregate Metrics"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "goose_scenario".into(),
            label: "Scenario".into(),
            description: "Load test scenario definition".into(),
            fields: vec![
                FieldDefinition::new("field_name", FieldType::Text { max_length: None })
                    .required()
                    .label("Name"),
                FieldDefinition::new("field_description", FieldType::TextLong).label("Description"),
                FieldDefinition::new(
                    "field_target_site_id",
                    FieldType::RecordReference("goose_site".into()),
                )
                .label("Target Site"),
                FieldDefinition::new("field_user_count", FieldType::Integer).label("User Count"),
                FieldDefinition::new("field_hatch_rate", FieldType::Float).label("Hatch Rate"),
                FieldDefinition::new("field_duration", FieldType::Integer).label("Duration"),
                FieldDefinition::new("field_task_config", FieldType::TextLong)
                    .label("Task Configuration"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "goose_endpoint_result".into(),
            label: "Endpoint Result".into(),
            description: "Per-endpoint metrics from a test run".into(),
            fields: vec![
                FieldDefinition::new(
                    "field_test_run_id",
                    FieldType::RecordReference("goose_test_run".into()),
                )
                .required()
                .label("Test Run"),
                FieldDefinition::new("field_url_pattern", FieldType::Text { max_length: None })
                    .required()
                    .label("URL Pattern"),
                FieldDefinition::new("field_method", FieldType::Text { max_length: None })
                    .required()
                    .label("HTTP Method"),
                FieldDefinition::new("field_request_count", FieldType::Integer)
                    .label("Request Count"),
                FieldDefinition::new("field_error_count", FieldType::Integer).label("Error Count"),
                FieldDefinition::new("field_avg_ms", FieldType::Float)
                    .label("Average Response (ms)"),
                FieldDefinition::new("field_p50", FieldType::Float).label("p50 (ms)"),
                FieldDefinition::new("field_p90", FieldType::Float).label("p90 (ms)"),
                FieldDefinition::new("field_p95", FieldType::Float).label("p95 (ms)"),
                FieldDefinition::new("field_p99", FieldType::Float).label("p99 (ms)"),
                FieldDefinition::new("field_rps", FieldType::Float).label("Requests Per Second"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "goose_site".into(),
            label: "Site".into(),
            description: "Target site for load testing".into(),
            fields: vec![
                FieldDefinition::new("field_name", FieldType::Text { max_length: None })
                    .required()
                    .label("Name"),
                FieldDefinition::new("field_base_url", FieldType::Text { max_length: None })
                    .required()
                    .label("Base URL"),
                FieldDefinition::new("field_description", FieldType::TextLong).label("Description"),
                FieldDefinition::new("field_environment", FieldType::Text { max_length: None })
                    .label("Environment"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "goose_comparison".into(),
            label: "Comparison".into(),
            description: "Side-by-side comparison of test runs".into(),
            fields: vec![
                FieldDefinition::new("field_name", FieldType::Text { max_length: None })
                    .required()
                    .label("Name"),
                FieldDefinition::new("field_run_ids", FieldType::TextLong).label("Run IDs"),
                FieldDefinition::new("field_annotations", FieldType::TextLong).label("Annotations"),
            ],
        },
    ]
}

const GOOSE_TYPES: &[&str] = &[
    "goose_test_run",
    "goose_scenario",
    "goose_endpoint_result",
    "goose_site",
    "goose_comparison",
];

/// Permissions: view / create / edit / delete for each of the 5 content types.
///
/// Permission format matches kernel fallback: "{operation} {type} content".
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    GOOSE_TYPES
        .iter()
        .flat_map(|t| PermissionDefinition::crud_for_type(t))
        .collect()
}

/// Menu routes: /test-runs and /sites listings.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/test-runs", "Test Runs")
            .callback("goose_run_list")
            .permission("access content"),
        MenuDefinition::new("/sites", "Sites")
            .callback("goose_site_list")
            .permission("access content"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_info_returns_five_types() {
        let types = __inner_tap_item_info();
        assert_eq!(types.len(), 5);
        let names: Vec<&str> = types.iter().map(|t| t.machine_name.as_str()).collect();
        assert!(names.contains(&"goose_test_run"));
        assert!(names.contains(&"goose_scenario"));
        assert!(names.contains(&"goose_endpoint_result"));
        assert!(names.contains(&"goose_site"));
        assert!(names.contains(&"goose_comparison"));
    }

    #[test]
    fn goose_endpoint_result_has_eleven_fields() {
        let types = __inner_tap_item_info();
        let result = types
            .iter()
            .find(|t| t.machine_name == "goose_endpoint_result")
            .unwrap();
        assert_eq!(result.fields.len(), 11);
    }

    #[test]
    fn perm_returns_twenty_permissions() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 20); // 4 per type × 5 types (view/create/edit/delete)
    }

    #[test]
    fn menu_returns_two_routes() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 2);
        assert_eq!(menus[0].path, "/test-runs");
        assert_eq!(menus[1].path, "/sites");
    }

    #[test]
    fn perm_format_matches_kernel_fallback() {
        let perms = __inner_tap_perm();
        for perm in &perms {
            assert!(
                !perm.name.contains(" any "),
                "permission '{}' must not contain 'any' — kernel fallback uses '{{op}} {{type}} content'",
                perm.name
            );
        }
    }
}
