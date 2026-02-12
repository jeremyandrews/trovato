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
    FilterOperator, FilterValue, GatherResult, GatherView, ViewDefinition, ViewDisplay, ViewFilter,
};
use anyhow::{Context, Result};
use dashmap::DashMap;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;

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
    ) -> Result<GatherResult> {
        // Apply exposed filters
        let resolved_definition = self
            .resolve_exposed_filters(definition.clone(), exposed_filters)
            .await?;

        // Resolve category hierarchy for HasTermOrDescendants filters
        let final_definition = self.resolve_category_hierarchies(resolved_definition).await?;

        // Build and execute queries
        let per_page = display.items_per_page;
        let builder = ViewQueryBuilder::new(final_definition, stage_id);

        // Execute count query
        let count_sql = builder.build_count();
        let total: i64 = sqlx::query_scalar(&count_sql)
            .fetch_one(&self.pool)
            .await
            .context("failed to execute count query")?;

        // Execute main query
        let main_sql = builder.build(page, per_page);
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(&format!(
            "SELECT row_to_json(t) FROM ({}) t",
            main_sql
        ))
        .fetch_all(&self.pool)
        .await
        .context("failed to execute main query")?;

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
            if filter.operator == FilterOperator::HasTermOrDescendants {
                // Expand to include all descendant tag IDs
                let tag_id = filter.value.as_uuid().ok_or_else(|| {
                    anyhow::anyhow!("HasTermOrDescendants requires UUID value")
                })?;

                let descendant_ids = self.categories.get_tag_with_descendants(tag_id).await?;

                // Replace with HasAnyTerm using expanded list
                resolved_filters.push(ViewFilter {
                    field: filter.field,
                    operator: FilterOperator::HasAnyTerm,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gather::types::{PagerConfig, PagerStyle, ViewSort, SortDirection};

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
}
