//! Gather extension registry for plugin-provided filters, relationships, and sorts.
//!
//! Plugins register extensions declaratively via `tap_gather_extend` JSON.
//! The kernel provides built-in handler implementations (Rust traits);
//! plugins activate and configure them by name.

use anyhow::Result;
use sea_query::{Order, SimpleExpr};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use uuid::Uuid;

use super::types::QueryFilter;

// ---------------------------------------------------------------------------
// Context structs
// ---------------------------------------------------------------------------

/// Context passed to filter handlers during SQL generation.
#[derive(Debug, Clone)]
pub struct FilterContext {
    /// Base table name (e.g. "item").
    pub base_table: String,
    /// Current stage UUID.
    pub stage_id: Uuid,
}

/// Context passed to relationship handlers during SQL generation.
#[derive(Debug, Clone)]
pub struct RelationshipContext {
    /// Base table name.
    pub base_table: String,
    /// Current stage UUID.
    pub stage_id: Uuid,
}

/// Context passed to sort handlers during SQL generation.
#[derive(Debug, Clone)]
pub struct SortContext {
    /// Base table name.
    pub base_table: String,
    /// Current stage UUID.
    pub stage_id: Uuid,
}

// ---------------------------------------------------------------------------
// Handler traits
// ---------------------------------------------------------------------------

/// Handler for custom filter operators.
///
/// Kernel-side Rust implementations that plugins activate via JSON config.
pub trait FilterHandler: Send + Sync {
    /// Build a SQL condition for this filter.
    fn build_condition(
        &self,
        filter: &QueryFilter,
        config: &serde_json::Value,
        ctx: &FilterContext,
    ) -> Result<Option<SimpleExpr>>;

    /// Optional async resolution phase (e.g. expand hierarchy IDs).
    ///
    /// Called before query building. Default implementation is a no-op
    /// that returns the filter unchanged.
    fn resolve<'a>(
        &'a self,
        filter: QueryFilter,
        config: &'a serde_json::Value,
        pool: &'a PgPool,
    ) -> Pin<Box<dyn Future<Output = Result<QueryFilter>> + Send + 'a>> {
        let _ = (config, pool);
        Box::pin(async move { Ok(filter) })
    }
}

/// Join specification returned by relationship handlers.
#[derive(Debug, Clone)]
pub struct JoinSpec {
    /// Target table to join.
    pub target_table: String,
    /// Alias for the joined table.
    pub alias: String,
    /// Join type (inner, left, right).
    pub join_type: super::types::JoinType,
    /// ON condition expression.
    pub on_condition: SimpleExpr,
}

/// Handler for custom relationship/join types.
pub trait RelationshipHandler: Send + Sync {
    /// Build a join specification.
    fn build_join(&self, config: &serde_json::Value, ctx: &RelationshipContext)
    -> Result<JoinSpec>;
}

/// Handler for custom sort operators.
pub trait SortHandler: Send + Sync {
    /// Build a sort expression and order direction.
    fn build_order(
        &self,
        sort: &super::types::QuerySort,
        config: &serde_json::Value,
        ctx: &SortContext,
    ) -> Result<(SimpleExpr, Order)>;
}

// ---------------------------------------------------------------------------
// Serde declaration types (from tap JSON)
// ---------------------------------------------------------------------------

/// Top-level declaration from a plugin's `tap_gather_extend` response.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GatherExtensionDeclaration {
    /// Custom filter extensions.
    #[serde(default)]
    pub filters: Vec<FilterExtension>,

    /// Custom relationship extensions.
    #[serde(default)]
    pub relationships: Vec<RelationshipExtension>,

    /// Custom sort extensions.
    #[serde(default)]
    pub sorts: Vec<SortExtension>,
}

/// A filter extension declaration from a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterExtension {
    /// Extension name used in `FilterOperator::Custom(name)`.
    pub name: String,
    /// Built-in handler name (e.g. "hierarchical_in", "jsonb_array_contains").
    pub handler: String,
    /// Handler-specific configuration.
    #[serde(default)]
    pub config: serde_json::Value,
}

/// A relationship extension declaration from a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipExtension {
    /// Extension name.
    pub name: String,
    /// Built-in handler name.
    pub handler: String,
    /// Handler-specific configuration.
    #[serde(default)]
    pub config: serde_json::Value,
}

/// A sort extension declaration from a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SortExtension {
    /// Extension name.
    pub name: String,
    /// Built-in handler name.
    pub handler: String,
    /// Handler-specific configuration.
    #[serde(default)]
    pub config: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Registered extension: maps an extension name to its handler + config.
struct FilterRegistration {
    handler_name: String,
    config: serde_json::Value,
}

struct RelationshipRegistration {
    handler_name: String,
    config: serde_json::Value,
}

struct SortRegistration {
    handler_name: String,
    config: serde_json::Value,
}

/// Validate an extension name: must be non-empty, alphanumeric/underscore/hyphen,
/// start with a letter or underscore, max 64 chars.
fn is_valid_extension_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
}

/// Registry for plugin-provided Gather extensions.
///
/// Two-level lookup: extension name → (handler name, config) → handler impl.
pub struct GatherExtensionRegistry {
    /// Built-in filter handler implementations, keyed by handler name.
    filter_handlers: HashMap<String, Box<dyn FilterHandler>>,
    /// Built-in relationship handler implementations.
    relationship_handlers: HashMap<String, Box<dyn RelationshipHandler>>,
    /// Built-in sort handler implementations.
    sort_handlers: HashMap<String, Box<dyn SortHandler>>,

    /// Registered filter extensions: extension name → registration.
    filter_extensions: HashMap<String, FilterRegistration>,
    /// Registered relationship extensions.
    relationship_extensions: HashMap<String, RelationshipRegistration>,
    /// Registered sort extensions.
    sort_extensions: HashMap<String, SortRegistration>,
}

impl Default for GatherExtensionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl GatherExtensionRegistry {
    /// Create a new registry with built-in handlers pre-registered.
    pub fn new() -> Self {
        let mut registry = Self {
            filter_handlers: HashMap::new(),
            relationship_handlers: HashMap::new(),
            sort_handlers: HashMap::new(),
            filter_extensions: HashMap::new(),
            relationship_extensions: HashMap::new(),
            sort_extensions: HashMap::new(),
        };

        // Register built-in filter handlers
        registry.register_filter_handler(
            "hierarchical_in",
            Box::new(super::handlers::HierarchicalInFilterHandler),
        );
        registry.register_filter_handler(
            "jsonb_array_contains",
            Box::new(super::handlers::JsonbArrayContainsFilterHandler),
        );

        registry
    }

    /// Register a built-in filter handler by name.
    pub fn register_filter_handler(&mut self, name: &str, handler: Box<dyn FilterHandler>) {
        self.filter_handlers.insert(name.to_string(), handler);
    }

    /// Register a built-in relationship handler by name.
    pub fn register_relationship_handler(
        &mut self,
        name: &str,
        handler: Box<dyn RelationshipHandler>,
    ) {
        self.relationship_handlers.insert(name.to_string(), handler);
    }

    /// Register a built-in sort handler by name.
    pub fn register_sort_handler(&mut self, name: &str, handler: Box<dyn SortHandler>) {
        self.sort_handlers.insert(name.to_string(), handler);
    }

    /// Apply plugin declarations, validating that referenced handlers exist.
    ///
    /// Each entry is `(plugin_name, declaration)`.
    pub fn apply_declarations(
        &mut self,
        declarations: Vec<(String, GatherExtensionDeclaration)>,
    ) -> Vec<String> {
        let mut warnings = Vec::new();

        for (plugin_name, decl) in declarations {
            for filter in decl.filters {
                if !is_valid_extension_name(&filter.name) {
                    warnings.push(format!(
                        "plugin '{}': filter name '{}' is invalid (must be alphanumeric/underscore/hyphen, start with letter or underscore)",
                        plugin_name, filter.name
                    ));
                    continue;
                }
                if !self.filter_handlers.contains_key(&filter.handler) {
                    warnings.push(format!(
                        "plugin '{}': filter '{}' references unknown handler '{}'",
                        plugin_name, filter.name, filter.handler
                    ));
                    continue;
                }
                if self.filter_extensions.contains_key(&filter.name) {
                    warnings.push(format!(
                        "plugin '{}': filter '{}' overwrites existing extension",
                        plugin_name, filter.name
                    ));
                }
                self.filter_extensions.insert(
                    filter.name,
                    FilterRegistration {
                        handler_name: filter.handler,
                        config: filter.config,
                    },
                );
            }

            for rel in decl.relationships {
                if !is_valid_extension_name(&rel.name) {
                    warnings.push(format!(
                        "plugin '{}': relationship name '{}' is invalid",
                        plugin_name, rel.name
                    ));
                    continue;
                }
                if !self.relationship_handlers.contains_key(&rel.handler) {
                    warnings.push(format!(
                        "plugin '{}': relationship '{}' references unknown handler '{}'",
                        plugin_name, rel.name, rel.handler
                    ));
                    continue;
                }
                if self.relationship_extensions.contains_key(&rel.name) {
                    warnings.push(format!(
                        "plugin '{}': relationship '{}' overwrites existing extension",
                        plugin_name, rel.name
                    ));
                }
                self.relationship_extensions.insert(
                    rel.name,
                    RelationshipRegistration {
                        handler_name: rel.handler,
                        config: rel.config,
                    },
                );
            }

            for sort in decl.sorts {
                if !is_valid_extension_name(&sort.name) {
                    warnings.push(format!(
                        "plugin '{}': sort name '{}' is invalid",
                        plugin_name, sort.name
                    ));
                    continue;
                }
                if !self.sort_handlers.contains_key(&sort.handler) {
                    warnings.push(format!(
                        "plugin '{}': sort '{}' references unknown handler '{}'",
                        plugin_name, sort.name, sort.handler
                    ));
                    continue;
                }
                if self.sort_extensions.contains_key(&sort.name) {
                    warnings.push(format!(
                        "plugin '{}': sort '{}' overwrites existing extension",
                        plugin_name, sort.name
                    ));
                }
                self.sort_extensions.insert(
                    sort.name,
                    SortRegistration {
                        handler_name: sort.handler,
                        config: sort.config,
                    },
                );
            }
        }

        warnings
    }

    /// Look up a filter extension, returning the handler and its config.
    pub fn get_filter(&self, name: &str) -> Option<(&dyn FilterHandler, &serde_json::Value)> {
        let reg = self.filter_extensions.get(name)?;
        let handler = self.filter_handlers.get(&reg.handler_name)?;
        Some((handler.as_ref(), &reg.config))
    }

    /// Look up a relationship extension.
    pub fn get_relationship(
        &self,
        name: &str,
    ) -> Option<(&dyn RelationshipHandler, &serde_json::Value)> {
        let reg = self.relationship_extensions.get(name)?;
        let handler = self.relationship_handlers.get(&reg.handler_name)?;
        Some((handler.as_ref(), &reg.config))
    }

    /// Look up a sort extension.
    pub fn get_sort(&self, name: &str) -> Option<(&dyn SortHandler, &serde_json::Value)> {
        let reg = self.sort_extensions.get(name)?;
        let handler = self.sort_handlers.get(&reg.handler_name)?;
        Some((handler.as_ref(), &reg.config))
    }

    /// Check if a filter extension is registered.
    pub fn has_filter(&self, name: &str) -> bool {
        self.filter_extensions.contains_key(name)
    }

    /// List all registered filter extension names.
    pub fn filter_names(&self) -> Vec<&str> {
        self.filter_extensions.keys().map(|s| s.as_str()).collect()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn declaration_serde_roundtrip() {
        let json = r#"{
            "filters": [{
                "name": "category_tag",
                "handler": "hierarchical_in",
                "config": {
                    "hierarchy_table": "category_tag_hierarchy",
                    "id_column": "tag_id",
                    "parent_column": "parent_id",
                    "expand_descendants": true
                }
            }],
            "relationships": [],
            "sorts": []
        }"#;

        let decl: GatherExtensionDeclaration = serde_json::from_str(json).unwrap();
        assert_eq!(decl.filters.len(), 1);
        assert_eq!(decl.filters[0].name, "category_tag");
        assert_eq!(decl.filters[0].handler, "hierarchical_in");
        assert!(decl.filters[0].config.get("expand_descendants").is_some());

        // Roundtrip
        let serialized = serde_json::to_string(&decl).unwrap();
        let parsed: GatherExtensionDeclaration = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed.filters.len(), 1);
    }

    #[test]
    fn declaration_defaults_empty_collections() {
        let json = "{}";
        let decl: GatherExtensionDeclaration = serde_json::from_str(json).unwrap();
        assert!(decl.filters.is_empty());
        assert!(decl.relationships.is_empty());
        assert!(decl.sorts.is_empty());
    }

    #[test]
    fn registry_new_has_builtin_handlers() {
        let registry = GatherExtensionRegistry::new();
        assert!(registry.filter_handlers.contains_key("hierarchical_in"));
        assert!(
            registry
                .filter_handlers
                .contains_key("jsonb_array_contains")
        );
    }

    #[test]
    fn registry_apply_valid_declarations() {
        let mut registry = GatherExtensionRegistry::new();

        let decl = GatherExtensionDeclaration {
            filters: vec![FilterExtension {
                name: "category_tag".to_string(),
                handler: "hierarchical_in".to_string(),
                config: serde_json::json!({"hierarchy_table": "category_tag_hierarchy"}),
            }],
            relationships: vec![],
            sorts: vec![],
        };

        let warnings = registry.apply_declarations(vec![("test_plugin".to_string(), decl)]);
        assert!(warnings.is_empty());
        assert!(registry.has_filter("category_tag"));
    }

    #[test]
    fn registry_apply_unknown_handler_warns() {
        let mut registry = GatherExtensionRegistry::new();

        let decl = GatherExtensionDeclaration {
            filters: vec![FilterExtension {
                name: "bad_filter".to_string(),
                handler: "nonexistent_handler".to_string(),
                config: serde_json::json!({}),
            }],
            relationships: vec![],
            sorts: vec![],
        };

        let warnings = registry.apply_declarations(vec![("bad_plugin".to_string(), decl)]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unknown handler"));
        assert!(!registry.has_filter("bad_filter"));
    }

    #[test]
    fn registry_get_filter_returns_handler_and_config() {
        let mut registry = GatherExtensionRegistry::new();

        let config = serde_json::json!({"hierarchy_table": "category_tag_hierarchy"});
        let decl = GatherExtensionDeclaration {
            filters: vec![FilterExtension {
                name: "my_filter".to_string(),
                handler: "hierarchical_in".to_string(),
                config: config.clone(),
            }],
            relationships: vec![],
            sorts: vec![],
        };

        registry.apply_declarations(vec![("plugin".to_string(), decl)]);

        let result = registry.get_filter("my_filter");
        assert!(result.is_some());
        let (_handler, returned_config) = result.unwrap();
        assert_eq!(returned_config, &config);
    }

    #[test]
    fn registry_get_nonexistent_filter_returns_none() {
        let registry = GatherExtensionRegistry::new();
        assert!(registry.get_filter("nonexistent").is_none());
    }

    #[test]
    fn registry_filter_names() {
        let mut registry = GatherExtensionRegistry::new();

        let decl = GatherExtensionDeclaration {
            filters: vec![
                FilterExtension {
                    name: "alpha".to_string(),
                    handler: "hierarchical_in".to_string(),
                    config: serde_json::json!({}),
                },
                FilterExtension {
                    name: "beta".to_string(),
                    handler: "jsonb_array_contains".to_string(),
                    config: serde_json::json!({}),
                },
            ],
            relationships: vec![],
            sorts: vec![],
        };

        registry.apply_declarations(vec![("plugin".to_string(), decl)]);

        let names = registry.filter_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
    }

    #[test]
    fn filter_extension_serde() {
        let ext = FilterExtension {
            name: "test".to_string(),
            handler: "hierarchical_in".to_string(),
            config: serde_json::json!({"key": "value"}),
        };

        let json = serde_json::to_string(&ext).unwrap();
        let parsed: FilterExtension = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "test");
        assert_eq!(parsed.handler, "hierarchical_in");
    }

    #[test]
    fn custom_filter_operator_serde() {
        use super::super::types::FilterOperator;

        let op = FilterOperator::Custom("category_tag".to_string());
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(json, r#"{"custom":"category_tag"}"#);

        let parsed: FilterOperator = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, FilterOperator::Custom("category_tag".to_string()));
    }

    #[test]
    fn registry_apply_invalid_name_warns() {
        let mut registry = GatherExtensionRegistry::new();

        let decl = GatherExtensionDeclaration {
            filters: vec![
                FilterExtension {
                    name: "".to_string(),
                    handler: "hierarchical_in".to_string(),
                    config: serde_json::json!({}),
                },
                FilterExtension {
                    name: "has spaces".to_string(),
                    handler: "hierarchical_in".to_string(),
                    config: serde_json::json!({}),
                },
                FilterExtension {
                    name: "123start".to_string(),
                    handler: "hierarchical_in".to_string(),
                    config: serde_json::json!({}),
                },
            ],
            relationships: vec![],
            sorts: vec![],
        };

        let warnings = registry.apply_declarations(vec![("plugin".to_string(), decl)]);
        assert_eq!(warnings.len(), 3);
        for w in &warnings {
            assert!(w.contains("invalid"));
        }
        assert!(registry.filter_names().is_empty());
    }

    #[test]
    fn registry_apply_collision_warns() {
        let mut registry = GatherExtensionRegistry::new();

        let decl1 = GatherExtensionDeclaration {
            filters: vec![FilterExtension {
                name: "my_filter".to_string(),
                handler: "hierarchical_in".to_string(),
                config: serde_json::json!({"version": 1}),
            }],
            relationships: vec![],
            sorts: vec![],
        };
        let decl2 = GatherExtensionDeclaration {
            filters: vec![FilterExtension {
                name: "my_filter".to_string(),
                handler: "jsonb_array_contains".to_string(),
                config: serde_json::json!({"version": 2}),
            }],
            relationships: vec![],
            sorts: vec![],
        };

        let warnings = registry.apply_declarations(vec![
            ("plugin_a".to_string(), decl1),
            ("plugin_b".to_string(), decl2),
        ]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("overwrites"));
        // The second registration wins
        assert!(registry.has_filter("my_filter"));
    }

    #[test]
    fn valid_extension_names() {
        assert!(is_valid_extension_name("category_tag"));
        assert!(is_valid_extension_name("_private"));
        assert!(is_valid_extension_name("my-filter"));
        assert!(is_valid_extension_name("a"));

        assert!(!is_valid_extension_name(""));
        assert!(!is_valid_extension_name("123abc"));
        assert!(!is_valid_extension_name("has spaces"));
        assert!(!is_valid_extension_name("has.dots"));
    }

    #[test]
    fn custom_filter_operator_backward_compat() {
        use super::super::types::FilterOperator;

        // Existing operators still work
        let json = r#""equals""#;
        let parsed: FilterOperator = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, FilterOperator::Equals);

        let json = r#""has_tag_or_descendants""#;
        let parsed: FilterOperator = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, FilterOperator::HasTagOrDescendants);
    }
}
