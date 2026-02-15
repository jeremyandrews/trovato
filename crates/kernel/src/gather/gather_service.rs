//! Gather service for executing view queries.
//!
//! Provides high-level query execution with:
//! - View registration and lookup
//! - Category hierarchy resolution
//! - Exposed filter handling
//! - Result caching

use super::category_service::CategoryService;
use super::query_builder::ViewQueryBuilder;
use super::types::{
    ContextualValue, FilterOperator, FilterValue, GatherResult, GatherView, QueryContext,
    ViewDefinition, ViewDisplay, ViewFilter,
};
use anyhow::{Context, Result};
use dashmap::DashMap;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

/// Maximum nesting depth for includes to prevent unbounded recursion.
const MAX_INCLUDE_DEPTH: u8 = 3;

/// Service for executing Gather queries.
pub struct GatherService {
    pool: PgPool,
    categories: Arc<CategoryService>,
    /// Registered views by view_id
    views: DashMap<String, GatherView>,
}

impl GatherService {
    /// Create a new GatherService.
    pub fn new(pool: PgPool, categories: Arc<CategoryService>) -> Arc<Self> {
        Arc::new(Self {
            pool,
            categories,
            views: DashMap::new(),
        })
    }

    /// Register a view definition.
    pub async fn register_view(&self, view: GatherView) -> Result<()> {
        let view_id = view.view_id.clone();

        // Persist to database
        let now = chrono::Utc::now().timestamp();
        let definition_json = serde_json::to_value(&view.definition)?;
        let display_json = serde_json::to_value(&view.display)?;

        sqlx::query(
            r#"
            INSERT INTO gather_view (view_id, label, description, definition, display, plugin, created, changed)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (view_id) DO UPDATE SET
                label = EXCLUDED.label,
                description = EXCLUDED.description,
                definition = EXCLUDED.definition,
                display = EXCLUDED.display,
                changed = EXCLUDED.changed
            "#,
        )
        .bind(&view.view_id)
        .bind(&view.label)
        .bind(&view.description)
        .bind(&definition_json)
        .bind(&display_json)
        .bind(&view.plugin)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("failed to register view")?;

        // Cache in memory
        self.views.insert(view_id, view);

        Ok(())
    }

    /// Get a view by ID.
    pub fn get_view(&self, view_id: &str) -> Option<GatherView> {
        self.views.get(view_id).map(|v| v.clone())
    }

    /// List all registered views.
    pub fn list_views(&self) -> Vec<GatherView> {
        self.views.iter().map(|v| v.clone()).collect()
    }

    /// Load views from database into memory cache.
    pub async fn load_views(&self) -> Result<()> {
        #[derive(sqlx::FromRow)]
        struct ViewRow {
            view_id: String,
            label: String,
            description: Option<String>,
            definition: serde_json::Value,
            display: serde_json::Value,
            plugin: String,
            created: i64,
            changed: i64,
        }

        let rows = sqlx::query_as::<_, ViewRow>(
            "SELECT view_id, label, description, definition, display, plugin, created, changed FROM gather_view",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to load views")?;

        for row in rows {
            let definition: ViewDefinition = serde_json::from_value(row.definition)
                .context("failed to parse view definition")?;
            let display: ViewDisplay =
                serde_json::from_value(row.display).context("failed to parse view display")?;

            let view = GatherView {
                view_id: row.view_id.clone(),
                label: row.label,
                description: row.description,
                definition,
                display,
                plugin: row.plugin,
                created: row.created,
                changed: row.changed,
            };

            self.views.insert(row.view_id, view);
        }

        Ok(())
    }

    /// Execute a registered view by ID.
    pub async fn execute(
        &self,
        view_id: &str,
        page: u32,
        exposed_filters: HashMap<String, FilterValue>,
        stage_id: &str,
        context: &QueryContext,
    ) -> Result<GatherResult> {
        let view = self
            .views
            .get(view_id)
            .ok_or_else(|| anyhow::anyhow!("view not found: {}", view_id))?;

        self.execute_definition(
            &view.definition,
            &view.display,
            page,
            exposed_filters,
            stage_id,
            context,
        )
        .await
    }

    /// Execute a view definition directly (for ad-hoc queries).
    pub async fn execute_definition(
        &self,
        definition: &ViewDefinition,
        display: &ViewDisplay,
        page: u32,
        exposed_filters: HashMap<String, FilterValue>,
        stage_id: &str,
        context: &QueryContext,
    ) -> Result<GatherResult> {
        // Apply exposed filters
        let resolved_definition = self
            .resolve_exposed_filters(definition.clone(), exposed_filters)
            .await?;

        // Resolve contextual values (CurrentUser, CurrentTime, UrlArg)
        let resolved_definition = Self::resolve_contextual_values(resolved_definition, context);

        // Resolve category hierarchy for HasTagOrDescendants filters
        let final_definition = self
            .resolve_category_hierarchies(resolved_definition)
            .await?;

        // Split includes from definition to avoid cloning the full tree
        // just for the query builder (which only uses filters/sorts/fields).
        let includes = final_definition.includes.clone();
        let builder_def = ViewDefinition {
            includes: HashMap::new(),
            ..final_definition
        };

        // Build and execute queries
        let per_page = display.items_per_page;
        let builder = ViewQueryBuilder::new(builder_def, stage_id);

        // Execute count query
        let count_sql = builder.build_count();
        let total: i64 = sqlx::query_scalar(&count_sql)
            .fetch_one(&self.pool)
            .await
            .context("failed to execute count query")?;

        // Execute main query
        let main_sql = builder.build(page, per_page);
        let mut rows: Vec<serde_json::Value> =
            sqlx::query_scalar(&format!("SELECT row_to_json(t) FROM ({}) t", main_sql))
                .fetch_all(&self.pool)
                .await
                .context("failed to execute main query")?;

        // Execute includes (batched sub-queries)
        if !includes.is_empty() {
            self.execute_includes(&mut rows, &includes, stage_id, context, 0)
                .await?;
        }

        Ok(GatherResult::new(rows, total as u64, page, per_page))
    }

    /// Apply exposed filter values from user input.
    async fn resolve_exposed_filters(
        &self,
        mut definition: ViewDefinition,
        exposed_values: HashMap<String, FilterValue>,
    ) -> Result<ViewDefinition> {
        for filter in &mut definition.filters {
            if filter.exposed {
                if let Some(value) = exposed_values.get(&filter.field) {
                    filter.value = value.clone();
                }
            }
        }
        Ok(definition)
    }

    /// Resolve category hierarchy filters by expanding tag IDs.
    async fn resolve_category_hierarchies(
        &self,
        mut definition: ViewDefinition,
    ) -> Result<ViewDefinition> {
        let mut resolved_filters = Vec::new();

        for filter in definition.filters {
            if filter.operator == FilterOperator::HasTagOrDescendants {
                // Expand to include all descendant tag IDs
                let tag_id = filter
                    .value
                    .as_uuid()
                    .ok_or_else(|| anyhow::anyhow!("HasTagOrDescendants requires UUID value"))?;

                let descendant_ids = self.categories.get_tag_with_descendants(tag_id).await?;

                // Replace with HasAnyTag using expanded list
                resolved_filters.push(ViewFilter {
                    field: filter.field,
                    operator: FilterOperator::HasAnyTag,
                    value: FilterValue::List(
                        descendant_ids.into_iter().map(FilterValue::Uuid).collect(),
                    ),
                    exposed: filter.exposed,
                    exposed_label: filter.exposed_label,
                });
            } else {
                resolved_filters.push(filter);
            }
        }

        definition.filters = resolved_filters;
        Ok(definition)
    }

    /// Resolve contextual values in filters, replacing `ContextualValue` variants
    /// with concrete `FilterValue`s based on the runtime context.
    fn resolve_contextual_values(
        mut definition: ViewDefinition,
        context: &QueryContext,
    ) -> ViewDefinition {
        for filter in &mut definition.filters {
            if let FilterValue::Contextual(ref ctx_val) = filter.value {
                filter.value = match ctx_val {
                    ContextualValue::CurrentUser => {
                        FilterValue::Uuid(context.current_user_id.unwrap_or(Uuid::nil()))
                    }
                    ContextualValue::CurrentTime => {
                        FilterValue::Integer(chrono::Utc::now().timestamp())
                    }
                    ContextualValue::UrlArg(name) => context
                        .url_args
                        .get(name)
                        .map(|v| FilterValue::String(v.clone()))
                        .unwrap_or(FilterValue::String(String::new())),
                };
            }
        }
        definition
    }

    /// Execute batched include sub-queries and distribute results into parent items.
    ///
    /// `depth` tracks recursion level; includes within includes are supported up to
    /// `MAX_INCLUDE_DEPTH` levels. Child contextual values are resolved per-include.
    fn execute_includes<'a>(
        &'a self,
        parent_items: &'a mut [serde_json::Value],
        includes: &'a HashMap<String, super::types::IncludeDefinition>,
        stage_id: &'a str,
        context: &'a QueryContext,
        depth: u8,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if depth >= MAX_INCLUDE_DEPTH {
                tracing::warn!(
                    depth,
                    "include depth limit ({}) reached, skipping nested includes",
                    MAX_INCLUDE_DEPTH
                );
                return Ok(());
            }

            for (include_name, include_def) in includes {
                // 1. Collect and deduplicate parent binding values
                let mut seen = HashSet::new();
                let parent_values: Vec<String> = parent_items
                    .iter()
                    .filter_map(|item| extract_field_value(item, &include_def.parent_field))
                    .filter(|v| seen.insert(v.clone()))
                    .collect();

                if parent_values.is_empty() {
                    // No parents to match — embed empty arrays/nulls
                    for item in parent_items.iter_mut() {
                        if let Some(obj) = item.as_object_mut() {
                            if include_def.singular {
                                obj.insert(include_name.clone(), serde_json::Value::Null);
                            } else {
                                obj.insert(include_name.clone(), serde_json::json!([]));
                            }
                        }
                    }
                    continue;
                }

                // 2. Build child query with In filter for batch loading
                let mut child_def = include_def.definition.clone();

                // Convert parent values to FilterValue list
                let filter_values: Vec<FilterValue> = parent_values
                    .iter()
                    .map(|v| {
                        if let Ok(uuid) = Uuid::parse_str(v) {
                            FilterValue::Uuid(uuid)
                        } else {
                            FilterValue::String(v.clone())
                        }
                    })
                    .collect();

                child_def.filters.push(ViewFilter {
                    field: include_def.child_field.clone(),
                    operator: FilterOperator::In,
                    value: FilterValue::List(filter_values),
                    exposed: false,
                    exposed_label: None,
                });

                // Resolve contextual values in child definition
                let child_def = Self::resolve_contextual_values(child_def, context);

                // Split child includes before executing (they recurse separately)
                let child_includes = child_def.includes.clone();
                let child_def_for_query = ViewDefinition {
                    includes: HashMap::new(),
                    ..child_def
                };

                // Default limit for child queries; warn if results may be truncated
                let default_child_limit: u32 = 1000;
                let child_display = include_def.display.clone().unwrap_or(ViewDisplay {
                    items_per_page: default_child_limit,
                    ..Default::default()
                });

                // 3. Execute child query (single batched query)
                let child_result = self
                    .execute_definition(
                        &child_def_for_query,
                        &child_display,
                        1,
                        HashMap::new(),
                        stage_id,
                        context,
                    )
                    .await
                    .context(format!("failed to execute include '{}'", include_name))?;

                if child_result.total > child_result.items.len() as u64 {
                    tracing::warn!(
                        include = %include_name,
                        returned = child_result.items.len(),
                        total = child_result.total,
                        "include results truncated; consider adding a display limit to the include definition"
                    );
                }

                // 4. Distribute child results into parent items
                let mut child_items: Vec<serde_json::Value> = child_result.items;

                // Recursively execute nested includes on child items
                if !child_includes.is_empty() {
                    self.execute_includes(
                        &mut child_items,
                        &child_includes,
                        stage_id,
                        context,
                        depth + 1,
                    )
                    .await?;
                }

                for item in parent_items.iter_mut() {
                    let parent_val = extract_field_value(item, &include_def.parent_field);

                    let matching: Vec<&serde_json::Value> = child_items
                        .iter()
                        .filter(|child| {
                            let child_val = extract_field_value(child, &include_def.child_field);
                            parent_val.is_some() && child_val == parent_val
                        })
                        .collect();

                    if let Some(obj) = item.as_object_mut() {
                        if include_def.singular {
                            obj.insert(
                                include_name.clone(),
                                matching
                                    .first()
                                    .map(|v| (*v).clone())
                                    .unwrap_or(serde_json::Value::Null),
                            );
                        } else {
                            obj.insert(
                                include_name.clone(),
                                serde_json::Value::Array(matching.into_iter().cloned().collect()),
                            );
                        }
                    }
                }
            }

            Ok(())
        })
    }

    /// Delete a view.
    pub async fn delete_view(&self, view_id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM gather_view WHERE view_id = $1")
            .bind(view_id)
            .execute(&self.pool)
            .await
            .context("failed to delete view")?;

        self.views.remove(view_id);

        Ok(result.rows_affected() > 0)
    }
}

/// Extract a string value from a JSON item by field path.
///
/// Handles top-level fields (`"id"`), single-level JSONB paths (`"fields.story_id"`),
/// and nested JSONB paths (`"fields.nested.deep"`). Returns `None` for null or
/// missing values to prevent false matches.
pub fn extract_field_value(item: &serde_json::Value, field_path: &str) -> Option<String> {
    if field_path.starts_with("fields.") {
        // JSONB path — the row_to_json result has a "fields" key with a JSON object
        let jsonb_path = &field_path[7..]; // strip "fields."
        let fields = item.get("fields")?;

        // Parse fields if it's a JSON string (some drivers return JSONB as text)
        let fields_obj = if fields.is_object() {
            std::borrow::Cow::Borrowed(fields)
        } else if let Some(s) = fields.as_str() {
            let parsed: serde_json::Value = serde_json::from_str(s).ok()?;
            std::borrow::Cow::Owned(parsed)
        } else {
            return None;
        };

        // Traverse nested path (e.g., "nested.deep" → fields["nested"]["deep"])
        let parts: Vec<&str> = jsonb_path.split('.').collect();
        let mut current = fields_obj.as_ref();
        for (i, part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                return current.get(part).and_then(json_value_to_string);
            } else {
                current = current.get(part)?;
            }
        }
        None
    } else {
        item.get(field_path).and_then(json_value_to_string)
    }
}

/// Convert a JSON value to its string representation for comparison.
/// Returns `None` for null values to prevent false matches.
fn json_value_to_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gather::types::{PagerConfig, PagerStyle, SortDirection, ViewSort};

    #[test]
    fn gather_result_pagination() {
        let result = GatherResult::new(vec![], 100, 5, 10);

        assert_eq!(result.total, 100);
        assert_eq!(result.page, 5);
        assert_eq!(result.per_page, 10);
        assert_eq!(result.total_pages, 10);
        assert!(result.has_prev);
        assert!(result.has_next);
    }

    #[test]
    fn gather_result_first_page() {
        let result = GatherResult::new(vec![], 100, 1, 10);

        assert!(!result.has_prev);
        assert!(result.has_next);
    }

    #[test]
    fn gather_result_last_page() {
        let result = GatherResult::new(vec![], 100, 10, 10);

        assert!(result.has_prev);
        assert!(!result.has_next);
    }

    #[test]
    fn gather_result_empty() {
        let result = GatherResult::empty(1, 10);

        assert_eq!(result.total, 0);
        assert_eq!(result.total_pages, 0);
        assert!(!result.has_prev);
        assert!(!result.has_next);
    }

    #[test]
    fn gather_view_serialization() {
        let view = GatherView {
            view_id: "recent_articles".to_string(),
            label: "Recent Articles".to_string(),
            description: Some("Shows recent blog posts".to_string()),
            definition: ViewDefinition {
                base_table: "item".to_string(),
                item_type: Some("blog".to_string()),
                sorts: vec![ViewSort {
                    field: "created".to_string(),
                    direction: SortDirection::Desc,
                    nulls: None,
                }],
                ..Default::default()
            },
            display: ViewDisplay {
                items_per_page: 10,
                pager: PagerConfig {
                    enabled: true,
                    style: PagerStyle::Full,
                    show_count: true,
                },
                ..Default::default()
            },
            plugin: "blog".to_string(),
            created: 1000,
            changed: 1000,
        };

        let json = serde_json::to_string(&view).unwrap();
        let parsed: GatherView = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.view_id, "recent_articles");
        assert_eq!(parsed.definition.item_type, Some("blog".to_string()));
    }

    #[test]
    fn extract_field_value_top_level() {
        let item = serde_json::json!({"id": "abc-123", "status": 1});
        assert_eq!(
            extract_field_value(&item, "id"),
            Some("abc-123".to_string())
        );
        assert_eq!(extract_field_value(&item, "status"), Some("1".to_string()));
        assert_eq!(extract_field_value(&item, "missing"), None);
    }

    #[test]
    fn extract_field_value_jsonb_path() {
        let item = serde_json::json!({
            "id": "story-1",
            "fields": {"story_id": "story-1", "score": 42}
        });
        assert_eq!(
            extract_field_value(&item, "fields.story_id"),
            Some("story-1".to_string())
        );
        assert_eq!(
            extract_field_value(&item, "fields.score"),
            Some("42".to_string())
        );
        assert_eq!(extract_field_value(&item, "fields.missing"), None);
    }

    #[test]
    fn extract_field_value_uuid() {
        let uuid = Uuid::nil();
        let item = serde_json::json!({"id": uuid.to_string()});
        assert_eq!(extract_field_value(&item, "id"), Some(uuid.to_string()));
    }

    #[test]
    fn extract_field_value_nested_jsonb_path() {
        let item = serde_json::json!({
            "fields": {"meta": {"source": "reuters", "priority": 5}}
        });
        assert_eq!(
            extract_field_value(&item, "fields.meta.source"),
            Some("reuters".to_string())
        );
        assert_eq!(
            extract_field_value(&item, "fields.meta.priority"),
            Some("5".to_string())
        );
        assert_eq!(extract_field_value(&item, "fields.meta.missing"), None);
    }

    #[test]
    fn extract_field_value_null_returns_none() {
        let item = serde_json::json!({"id": null, "fields": {"story_id": null}});
        assert_eq!(extract_field_value(&item, "id"), None);
        assert_eq!(extract_field_value(&item, "fields.story_id"), None);
    }

    #[test]
    fn resolve_contextual_current_user() {
        let user_id = Uuid::now_v7();
        let context = QueryContext {
            current_user_id: Some(user_id),
            url_args: HashMap::new(),
        };

        let def = ViewDefinition {
            filters: vec![ViewFilter {
                field: "fields.user_id".to_string(),
                operator: FilterOperator::Equals,
                value: FilterValue::Contextual(ContextualValue::CurrentUser),
                exposed: false,
                exposed_label: None,
            }],
            ..Default::default()
        };

        let resolved = GatherService::resolve_contextual_values(def, &context);
        match &resolved.filters[0].value {
            FilterValue::Uuid(u) => assert_eq!(*u, user_id),
            other => panic!("expected Uuid, got {:?}", other),
        }
    }

    #[test]
    fn resolve_contextual_current_user_anonymous() {
        let context = QueryContext::default();

        let def = ViewDefinition {
            filters: vec![ViewFilter {
                field: "fields.user_id".to_string(),
                operator: FilterOperator::Equals,
                value: FilterValue::Contextual(ContextualValue::CurrentUser),
                exposed: false,
                exposed_label: None,
            }],
            ..Default::default()
        };

        let resolved = GatherService::resolve_contextual_values(def, &context);
        match &resolved.filters[0].value {
            FilterValue::Uuid(u) => assert_eq!(*u, Uuid::nil()),
            other => panic!("expected nil Uuid, got {:?}", other),
        }
    }

    #[test]
    fn resolve_contextual_current_time() {
        let context = QueryContext::default();
        let before = chrono::Utc::now().timestamp();

        let def = ViewDefinition {
            filters: vec![ViewFilter {
                field: "created".to_string(),
                operator: FilterOperator::LessThan,
                value: FilterValue::Contextual(ContextualValue::CurrentTime),
                exposed: false,
                exposed_label: None,
            }],
            ..Default::default()
        };

        let resolved = GatherService::resolve_contextual_values(def, &context);
        let after = chrono::Utc::now().timestamp();

        match &resolved.filters[0].value {
            FilterValue::Integer(ts) => {
                assert!(*ts >= before && *ts <= after);
            }
            other => panic!("expected Integer, got {:?}", other),
        }
    }

    #[test]
    fn resolve_contextual_url_arg() {
        let mut url_args = HashMap::new();
        url_args.insert("category".to_string(), "tech".to_string());
        let context = QueryContext {
            current_user_id: None,
            url_args,
        };

        let def = ViewDefinition {
            filters: vec![ViewFilter {
                field: "fields.category".to_string(),
                operator: FilterOperator::Equals,
                value: FilterValue::Contextual(ContextualValue::UrlArg("category".to_string())),
                exposed: false,
                exposed_label: None,
            }],
            ..Default::default()
        };

        let resolved = GatherService::resolve_contextual_values(def, &context);
        match &resolved.filters[0].value {
            FilterValue::String(s) => assert_eq!(s, "tech"),
            other => panic!("expected String, got {:?}", other),
        }
    }
}
