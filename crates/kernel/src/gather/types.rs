//! Gather query engine types.
//!
//! Provides type definitions for the declarative query builder:
//! - QueryDefinition: Query specification (filters, sorts, fields)
//! - QueryDisplay: Rendering configuration (format, pager)
//! - FilterOperator: Comparison operators including category-aware filters

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Complete query definition for Gather queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryDefinition {
    /// Base table to query (typically "item").
    #[serde(default = "default_base_table")]
    pub base_table: String,

    /// Filter by content type (optional).
    pub item_type: Option<String>,

    /// Fields to select.
    #[serde(default)]
    pub fields: Vec<QueryField>,

    /// Filter conditions.
    #[serde(default)]
    pub filters: Vec<QueryFilter>,

    /// Sort order.
    #[serde(default)]
    pub sorts: Vec<QuerySort>,

    /// Join relationships.
    #[serde(default)]
    pub relationships: Vec<QueryRelationship>,

    /// Named sub-queries to nest into parent results.
    #[serde(default)]
    pub includes: HashMap<String, IncludeDefinition>,
}

fn default_base_table() -> String {
    "item".to_string()
}

impl Default for QueryDefinition {
    fn default() -> Self {
        Self {
            base_table: default_base_table(),
            item_type: None,
            fields: Vec::new(),
            filters: Vec::new(),
            sorts: Vec::new(),
            relationships: Vec::new(),
            includes: HashMap::new(),
        }
    }
}

/// Field to select in the query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryField {
    /// Field name (can use dots for JSONB paths: "fields.body").
    pub field_name: String,

    /// Optional table alias for joins.
    pub table_alias: Option<String>,

    /// Display label.
    pub label: Option<String>,
}

/// Filter condition for queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryFilter {
    /// Field to filter on.
    pub field: String,

    /// Comparison operator.
    pub operator: FilterOperator,

    /// Value to compare against.
    pub value: FilterValue,

    /// Whether user can modify this filter.
    #[serde(default)]
    pub exposed: bool,

    /// Label for exposed filter UI.
    pub exposed_label: Option<String>,
}

/// Comparison operators for filtering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FilterOperator {
    /// Exact match.
    Equals,
    /// Not equal.
    NotEquals,
    /// Substring match (ILIKE %value%).
    Contains,
    /// Prefix match (ILIKE value%).
    StartsWith,
    /// Suffix match (ILIKE %value).
    EndsWith,
    /// Greater than.
    GreaterThan,
    /// Less than.
    LessThan,
    /// Greater than or equal.
    GreaterOrEqual,
    /// Less than or equal.
    LessOrEqual,
    /// Value in list.
    In,
    /// Value not in list.
    NotIn,
    /// Field is NULL.
    IsNull,
    /// Field is not NULL.
    IsNotNull,
    /// Has exact category tag.
    #[serde(rename = "has_tag")]
    HasTag,
    /// Has any of the specified tags.
    #[serde(rename = "has_any_tag")]
    HasAnyTag,
    /// Has all of the specified tags.
    #[serde(rename = "has_all_tags")]
    HasAllTags,
    /// Has tag or any of its descendants (hierarchical filter).
    #[serde(rename = "has_tag_or_descendants")]
    HasTagOrDescendants,
    /// Custom filter provided by a plugin extension.
    #[serde(rename = "custom")]
    Custom(String),
}

/// Filter value types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FilterValue {
    /// String value.
    String(String),
    /// Integer value.
    Integer(i64),
    /// Float value.
    Float(f64),
    /// Boolean value.
    Boolean(bool),
    /// UUID value.
    Uuid(Uuid),
    /// List of values (for In/NotIn operators).
    List(Vec<FilterValue>),
    /// Contextual value resolved at query time.
    Contextual(ContextualValue),
}

impl FilterValue {
    /// Convert to string representation for SQL.
    pub fn as_string(&self) -> Option<String> {
        match self {
            FilterValue::String(s) => Some(s.clone()),
            FilterValue::Integer(i) => Some(i.to_string()),
            FilterValue::Float(f) => Some(f.to_string()),
            FilterValue::Boolean(b) => Some(b.to_string()),
            FilterValue::Uuid(u) => Some(u.to_string()),
            _ => None,
        }
    }

    /// Convert to integer if possible.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            FilterValue::Integer(i) => Some(*i),
            FilterValue::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Convert to UUID if possible.
    pub fn as_uuid(&self) -> Option<Uuid> {
        match self {
            FilterValue::Uuid(u) => Some(*u),
            FilterValue::String(s) => Uuid::parse_str(s).ok(),
            _ => None,
        }
    }

    /// Extract list of UUIDs (for category filters).
    pub fn as_uuid_list(&self) -> Vec<Uuid> {
        match self {
            FilterValue::Uuid(u) => vec![*u],
            FilterValue::List(items) => items.iter().filter_map(|v| v.as_uuid()).collect(),
            FilterValue::String(s) => Uuid::parse_str(s).map(|u| vec![u]).unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

/// Contextual values resolved at query time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextualValue {
    /// Current authenticated user's ID.
    CurrentUser,
    /// Current Unix timestamp.
    CurrentTime,
    /// Value from URL argument.
    UrlArg(String),
}

/// Nested sub-query specification for composite responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncludeDefinition {
    /// Child query definition.
    pub definition: QueryDefinition,

    /// Field on the parent item to match (e.g., "id").
    pub parent_field: String,

    /// Field on the child item to match (e.g., "fields.story_id").
    pub child_field: String,

    /// If true, embed as a single object; if false, embed as an array.
    #[serde(default)]
    pub singular: bool,

    /// Optional display/pagination for the child query.
    pub display: Option<QueryDisplay>,
}

/// Runtime context for query execution.
#[derive(Debug, Clone, Default)]
pub struct QueryContext {
    /// Current authenticated user's ID (None for anonymous).
    pub current_user_id: Option<Uuid>,

    /// URL arguments available for UrlArg resolution.
    pub url_args: HashMap<String, String>,
}

/// Sort specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuerySort {
    /// Field to sort by.
    pub field: String,

    /// Sort direction.
    #[serde(default)]
    pub direction: SortDirection,

    /// NULL handling.
    pub nulls: Option<NullsOrder>,
}

/// Sort direction.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SortDirection {
    #[default]
    Asc,
    Desc,
}

/// NULL ordering preference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NullsOrder {
    First,
    Last,
}

/// Relationship/join specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRelationship {
    /// Relationship name (used as table alias).
    pub name: String,

    /// Target table to join.
    pub target_table: String,

    /// Join type.
    #[serde(default)]
    pub join_type: JoinType,

    /// Local field for join condition.
    pub local_field: String,

    /// Foreign field for join condition.
    pub foreign_field: String,
}

/// SQL join types.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JoinType {
    #[default]
    Inner,
    Left,
    Right,
}

/// Display configuration for rendering results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryDisplay {
    /// Output format.
    #[serde(default)]
    pub format: DisplayFormat,

    /// Number of items per page.
    #[serde(default = "default_items_per_page")]
    pub items_per_page: u32,

    /// Pager configuration.
    #[serde(default)]
    pub pager: PagerConfig,

    /// Text to show when results are empty.
    pub empty_text: Option<String>,

    /// Header content.
    pub header: Option<String>,

    /// Footer content.
    pub footer: Option<String>,
}

fn default_items_per_page() -> u32 {
    10
}

impl Default for QueryDisplay {
    fn default() -> Self {
        Self {
            format: DisplayFormat::default(),
            items_per_page: default_items_per_page(),
            pager: PagerConfig::default(),
            empty_text: None,
            header: None,
            footer: None,
        }
    }
}

/// Display format options.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DisplayFormat {
    #[default]
    Table,
    List,
    Grid,
    /// Custom template name.
    #[serde(rename = "custom")]
    Custom(String),
}

impl DisplayFormat {
    /// Get the format as a string for template names.
    pub fn as_str(&self) -> &str {
        match self {
            DisplayFormat::Table => "table",
            DisplayFormat::List => "list",
            DisplayFormat::Grid => "grid",
            DisplayFormat::Custom(name) => name.as_str(),
        }
    }
}

/// Pager configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PagerConfig {
    /// Whether paging is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Pager style.
    #[serde(default)]
    pub style: PagerStyle,

    /// Whether to show total count.
    #[serde(default = "default_true")]
    pub show_count: bool,
}

fn default_true() -> bool {
    true
}

impl Default for PagerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            style: PagerStyle::default(),
            show_count: true,
        }
    }
}

/// Pager display styles.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PagerStyle {
    /// Full pager: First, Prev, page numbers, Next, Last.
    #[default]
    Full,
    /// Mini pager: Prev/Next only.
    Mini,
    /// Infinite scroll / load more.
    Infinite,
}

/// Complete gather query (definition + display + metadata).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatherQuery {
    /// Unique query identifier.
    pub query_id: String,

    /// Human-readable label.
    pub label: String,

    /// Optional description.
    pub description: Option<String>,

    /// Query definition.
    pub definition: QueryDefinition,

    /// Display configuration.
    pub display: QueryDisplay,

    /// Owning plugin.
    pub plugin: String,

    /// Unix timestamp when created.
    #[serde(default)]
    pub created: i64,

    /// Unix timestamp when last changed.
    #[serde(default)]
    pub changed: i64,
}

impl Default for GatherQuery {
    fn default() -> Self {
        Self {
            query_id: String::new(),
            label: String::new(),
            description: None,
            definition: QueryDefinition::default(),
            display: QueryDisplay::default(),
            plugin: "core".to_string(),
            created: 0,
            changed: 0,
        }
    }
}

/// Result from executing a gather query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatherResult {
    /// Query results as JSON values.
    pub items: Vec<serde_json::Value>,

    /// Total count (before paging).
    pub total: u64,

    /// Current page number (1-indexed).
    pub page: u32,

    /// Items per page.
    pub per_page: u32,

    /// Total number of pages.
    pub total_pages: u32,

    /// Whether there's a next page.
    pub has_next: bool,

    /// Whether there's a previous page.
    pub has_prev: bool,
}

impl GatherResult {
    /// Create a new result with paging calculations.
    pub fn new(items: Vec<serde_json::Value>, total: u64, page: u32, per_page: u32) -> Self {
        let total_pages = if per_page > 0 {
            ((total as f64) / (per_page as f64)).ceil() as u32
        } else {
            1
        };

        Self {
            items,
            total,
            page,
            per_page,
            total_pages,
            has_next: page < total_pages,
            has_prev: page > 1,
        }
    }

    /// Create an empty result.
    #[allow(dead_code)]
    pub fn empty(page: u32, per_page: u32) -> Self {
        Self {
            items: Vec::new(),
            total: 0,
            page,
            per_page,
            total_pages: 0,
            has_next: false,
            has_prev: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_definition_defaults() {
        let def = QueryDefinition::default();
        assert_eq!(def.base_table, "item");
        assert!(def.filters.is_empty());
        assert!(def.sorts.is_empty());
    }

    #[test]
    fn filter_value_conversions() {
        let str_val = FilterValue::String("hello".to_string());
        assert_eq!(str_val.as_string(), Some("hello".to_string()));

        let int_val = FilterValue::Integer(42);
        assert_eq!(int_val.as_i64(), Some(42));

        let uuid = Uuid::nil();
        let uuid_val = FilterValue::Uuid(uuid);
        assert_eq!(uuid_val.as_uuid(), Some(uuid));
    }

    #[test]
    fn filter_value_uuid_list() {
        let uuid1 = Uuid::nil();
        let uuid2 = Uuid::now_v7();

        let single = FilterValue::Uuid(uuid1);
        assert_eq!(single.as_uuid_list().len(), 1);

        let list = FilterValue::List(vec![FilterValue::Uuid(uuid1), FilterValue::Uuid(uuid2)]);
        assert_eq!(list.as_uuid_list().len(), 2);
    }

    #[test]
    fn filter_operator_serialization() {
        let op = FilterOperator::HasTagOrDescendants;
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(json, "\"has_tag_or_descendants\"");

        let parsed: FilterOperator = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, FilterOperator::HasTagOrDescendants);
    }

    #[test]
    fn query_display_defaults() {
        let display = QueryDisplay::default();
        assert_eq!(display.items_per_page, 10);
        assert!(display.pager.enabled);
    }

    #[test]
    fn gather_result_paging() {
        let result = GatherResult::new(vec![serde_json::json!({"id": 1})], 25, 2, 10);

        assert_eq!(result.total, 25);
        assert_eq!(result.page, 2);
        assert_eq!(result.total_pages, 3);
        assert!(result.has_next);
        assert!(result.has_prev);
    }

    #[test]
    fn gather_result_last_page() {
        let result = GatherResult::new(vec![], 25, 3, 10);

        assert!(!result.has_next);
        assert!(result.has_prev);
    }

    #[test]
    fn gather_result_single_page() {
        let result = GatherResult::new(vec![], 5, 1, 10);

        assert!(!result.has_next);
        assert!(!result.has_prev);
        assert_eq!(result.total_pages, 1);
    }

    #[test]
    fn query_definition_serialization() {
        let def = QueryDefinition {
            base_table: "item".to_string(),
            item_type: Some("blog".to_string()),
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
            includes: HashMap::new(),
        };

        let json = serde_json::to_string(&def).unwrap();
        let parsed: QueryDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.item_type, Some("blog".to_string()));
    }

    #[test]
    fn include_definition_serde_roundtrip() {
        let include = IncludeDefinition {
            definition: QueryDefinition {
                item_type: Some("article".to_string()),
                ..Default::default()
            },
            parent_field: "id".to_string(),
            child_field: "fields.story_id".to_string(),
            singular: false,
            display: None,
        };

        let json = serde_json::to_string(&include).unwrap();
        let parsed: IncludeDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.parent_field, "id");
        assert_eq!(parsed.child_field, "fields.story_id");
        assert!(!parsed.singular);
        assert_eq!(parsed.definition.item_type, Some("article".to_string()));
    }

    #[test]
    fn include_definition_singular_roundtrip() {
        let include = IncludeDefinition {
            definition: QueryDefinition {
                item_type: Some("reaction".to_string()),
                ..Default::default()
            },
            parent_field: "id".to_string(),
            child_field: "fields.item_id".to_string(),
            singular: true,
            display: None,
        };

        let json = serde_json::to_string(&include).unwrap();
        let parsed: IncludeDefinition = serde_json::from_str(&json).unwrap();
        assert!(parsed.singular);
    }

    #[test]
    fn query_definition_with_includes_roundtrip() {
        let mut includes = HashMap::new();
        includes.insert(
            "articles".to_string(),
            IncludeDefinition {
                definition: QueryDefinition {
                    item_type: Some("article".to_string()),
                    ..Default::default()
                },
                parent_field: "id".to_string(),
                child_field: "fields.story_id".to_string(),
                singular: false,
                display: None,
            },
        );

        let def = QueryDefinition {
            item_type: Some("story".to_string()),
            includes,
            ..Default::default()
        };

        let json = serde_json::to_string(&def).unwrap();
        let parsed: QueryDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.includes.len(), 1);
        assert!(parsed.includes.contains_key("articles"));
    }

    #[test]
    fn query_definition_without_includes_deserializes() {
        // Backward compatibility: JSON without "includes" should parse fine
        let json = r#"{"item_type":"blog"}"#;
        let parsed: QueryDefinition = serde_json::from_str(json).unwrap();
        assert!(parsed.includes.is_empty());
        assert_eq!(parsed.item_type, Some("blog".to_string()));
    }
}
