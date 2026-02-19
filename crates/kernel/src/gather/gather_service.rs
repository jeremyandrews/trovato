//! Gather service for executing queries.
//!
//! Provides high-level query execution with:
//! - Query registration and lookup
//! - Category hierarchy resolution
//! - Exposed filter handling
//! - Result caching

use super::category_service::CategoryService;
use super::extension::GatherExtensionRegistry;
use super::query_builder::GatherQueryBuilder;
use super::types::{
    ContextualValue, FilterOperator, FilterValue, GatherQuery, GatherResult, QueryContext,
    QueryDefinition, QueryDisplay, QueryFilter,
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
    extensions: Arc<GatherExtensionRegistry>,
    /// Registered queries by query_id
    queries: DashMap<String, GatherQuery>,
}

impl GatherService {
    /// Create a new GatherService.
    pub fn new(
        pool: PgPool,
        categories: Arc<CategoryService>,
        extensions: Arc<GatherExtensionRegistry>,
    ) -> Arc<Self> {
        Arc::new(Self {
            pool,
            categories,
            extensions,
            queries: DashMap::new(),
        })
    }

    /// Register a query definition.
    pub async fn register_query(&self, query: GatherQuery) -> Result<()> {
        let query_id = query.query_id.clone();

        // Persist to database
        let now = chrono::Utc::now().timestamp();
        let definition_json = serde_json::to_value(&query.definition)?;
        let display_json = serde_json::to_value(&query.display)?;

        sqlx::query(
            r#"
            INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (query_id) DO UPDATE SET
                label = EXCLUDED.label,
                description = EXCLUDED.description,
                definition = EXCLUDED.definition,
                display = EXCLUDED.display,
                changed = EXCLUDED.changed
            "#,
        )
        .bind(&query.query_id)
        .bind(&query.label)
        .bind(&query.description)
        .bind(&definition_json)
        .bind(&display_json)
        .bind(&query.plugin)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("failed to register query")?;

        // Cache in memory
        self.queries.insert(query_id, query);

        Ok(())
    }

    /// Get a query by ID.
    pub fn get_query(&self, query_id: &str) -> Option<GatherQuery> {
        self.queries.get(query_id).map(|v| v.clone())
    }

    /// List all registered queries.
    pub fn list_queries(&self) -> Vec<GatherQuery> {
        self.queries.iter().map(|v| v.clone()).collect()
    }

    /// Load queries from database into memory cache.
    pub async fn load_queries(&self) -> Result<()> {
        #[derive(sqlx::FromRow)]
        struct QueryRow {
            query_id: String,
            label: String,
            description: Option<String>,
            definition: serde_json::Value,
            display: serde_json::Value,
            plugin: String,
            created: i64,
            changed: i64,
        }

        let rows = sqlx::query_as::<_, QueryRow>(
            "SELECT query_id, label, description, definition, display, plugin, created, changed FROM gather_query",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to load queries")?;

        for row in rows {
            let definition: QueryDefinition = serde_json::from_value(row.definition).context(
                format!("failed to parse query definition for '{}'", row.query_id),
            )?;
            let display: QueryDisplay = serde_json::from_value(row.display).context(format!(
                "failed to parse query display for '{}'",
                row.query_id
            ))?;

            let query = GatherQuery {
                query_id: row.query_id.clone(),
                label: row.label,
                description: row.description,
                definition,
                display,
                plugin: row.plugin,
                created: row.created,
                changed: row.changed,
            };

            self.queries.insert(row.query_id, query);
        }

        Ok(())
    }

    /// Execute a registered query by ID.
    pub async fn execute(
        &self,
        query_id: &str,
        page: u32,
        exposed_filters: HashMap<String, FilterValue>,
        stage_id: &str,
        context: &QueryContext,
    ) -> Result<GatherResult> {
        self.execute_with_stages(
            query_id,
            page,
            exposed_filters,
            &[stage_id.to_string()],
            context,
        )
        .await
    }

    /// Execute a registered query with stage hierarchy overlay.
    ///
    /// `stage_ids` is the ancestry chain (e.g., `["review", "draft", "live"]`).
    /// Items in any of these stages will be included in results.
    pub async fn execute_with_stages(
        &self,
        query_id: &str,
        page: u32,
        exposed_filters: HashMap<String, FilterValue>,
        stage_ids: &[String],
        context: &QueryContext,
    ) -> Result<GatherResult> {
        let query = self
            .queries
            .get(query_id)
            .ok_or_else(|| anyhow::anyhow!("query not found: {query_id}"))?;

        self.execute_definition_with_stages(
            &query.definition,
            &query.display,
            page,
            exposed_filters,
            stage_ids,
            context,
        )
        .await
    }

    /// Execute a query definition directly (for ad-hoc queries).
    pub async fn execute_definition(
        &self,
        definition: &QueryDefinition,
        display: &QueryDisplay,
        page: u32,
        exposed_filters: HashMap<String, FilterValue>,
        stage_id: &str,
        context: &QueryContext,
    ) -> Result<GatherResult> {
        self.execute_definition_with_stages(
            definition,
            display,
            page,
            exposed_filters,
            &[stage_id.to_string()],
            context,
        )
        .await
    }

    /// Execute a query definition with stage hierarchy overlay.
    pub async fn execute_definition_with_stages(
        &self,
        definition: &QueryDefinition,
        display: &QueryDisplay,
        page: u32,
        exposed_filters: HashMap<String, FilterValue>,
        stage_ids: &[String],
        context: &QueryContext,
    ) -> Result<GatherResult> {
        // Performance guardrails: validate definition before execution
        let validation_errors = Self::validate_definition(definition);
        if !validation_errors.is_empty() {
            anyhow::bail!("Query validation failed: {}", validation_errors.join("; "));
        }

        // Cap items_per_page to MAX_ITEMS_PER_PAGE
        let resolved_display = if display.items_per_page > MAX_ITEMS_PER_PAGE {
            let requested = display.items_per_page;
            tracing::warn!(
                requested = requested,
                capped = MAX_ITEMS_PER_PAGE,
                "items_per_page exceeds maximum, capping"
            );
            let mut capped = display.clone();
            capped.items_per_page = MAX_ITEMS_PER_PAGE;
            capped
        } else {
            display.clone()
        };
        let display = &resolved_display;

        // Apply exposed filters
        let resolved_definition = self
            .resolve_exposed_filters(definition.clone(), exposed_filters)
            .await?;

        // Resolve contextual values (CurrentUser, CurrentTime, UrlArg)
        let resolved_definition = Self::resolve_contextual_values(resolved_definition, context);

        // Resolve category hierarchy for HasTagOrDescendants filters
        let resolved_definition = self
            .resolve_category_hierarchies(resolved_definition)
            .await?;

        // Resolve custom filter extensions (expand hierarchies, etc.)
        let final_definition = self.resolve_custom_filters(resolved_definition).await?;

        // Split includes from definition to avoid cloning the full tree
        // just for the query builder (which only uses filters/sorts/fields).
        let includes = final_definition.includes.clone();
        let builder_def = QueryDefinition {
            includes: HashMap::new(),
            ..final_definition
        };

        // Build and execute queries
        let per_page = display.items_per_page;
        let builder = GatherQueryBuilder::new_with_stages(builder_def, stage_ids.to_vec())
            .with_extensions(self.extensions.clone());

        // Execute count and main queries with a statement timeout for safety.
        // Use a transaction so SET LOCAL applies correctly and resets on commit/rollback.
        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to begin transaction")?;

        // Set statement timeout (10 seconds) within this transaction
        sqlx::query("SET LOCAL statement_timeout = '10s'")
            .execute(&mut *tx)
            .await
            .context("failed to set statement timeout")?;

        let count_sql = builder.build_count();
        let total: i64 = sqlx::query_scalar(&count_sql)
            .fetch_one(&mut *tx)
            .await
            .context("failed to execute count query")?;

        let main_sql = builder.build(page, per_page);
        let mut rows: Vec<serde_json::Value> =
            sqlx::query_scalar(&format!("SELECT row_to_json(t) FROM ({main_sql}) t"))
                .fetch_all(&mut *tx)
                .await
                .context("failed to execute main query")?;

        // Commit transaction (SET LOCAL resets automatically)
        tx.commit()
            .await
            .context("failed to commit query transaction")?;

        // Execute includes (batched sub-queries)
        if !includes.is_empty() {
            self.execute_includes(&mut rows, &includes, stage_ids, context, 0)
                .await?;
        }

        Ok(GatherResult::new(rows, total as u64, page, per_page))
    }

    /// Apply exposed filter values from user input.
    async fn resolve_exposed_filters(
        &self,
        mut definition: QueryDefinition,
        exposed_values: HashMap<String, FilterValue>,
    ) -> Result<QueryDefinition> {
        for filter in &mut definition.filters {
            if filter.exposed
                && let Some(value) = exposed_values.get(&filter.field)
            {
                filter.value = value.clone();
            }
        }
        Ok(definition)
    }

    /// Resolve category hierarchy filters by expanding tag IDs.
    async fn resolve_category_hierarchies(
        &self,
        mut definition: QueryDefinition,
    ) -> Result<QueryDefinition> {
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
                resolved_filters.push(QueryFilter {
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

    /// Resolve custom filter extensions by calling each handler's resolve phase.
    async fn resolve_custom_filters(
        &self,
        mut definition: QueryDefinition,
    ) -> Result<QueryDefinition> {
        let mut resolved_filters = Vec::new();

        for filter in definition.filters {
            if let FilterOperator::Custom(ref name) = filter.operator {
                let name = name.clone();
                if let Some((handler, config)) = self.extensions.get_filter(&name) {
                    let resolved = handler
                        .resolve(filter, config, &self.pool)
                        .await
                        .context(format!("failed to resolve custom filter '{name}'"))?;
                    resolved_filters.push(resolved);
                } else {
                    resolved_filters.push(filter);
                }
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
        mut definition: QueryDefinition,
        context: &QueryContext,
    ) -> QueryDefinition {
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
        stage_ids: &'a [String],
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

                child_def.filters.push(QueryFilter {
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
                let child_def_for_query = QueryDefinition {
                    includes: HashMap::new(),
                    ..child_def
                };

                // Default limit for child queries; warn if results may be truncated
                let default_child_limit: u32 = 1000;
                let child_display = include_def.display.clone().unwrap_or(QueryDisplay {
                    items_per_page: default_child_limit,
                    ..Default::default()
                });

                // 3. Execute child query (single batched query)
                let child_result = self
                    .execute_definition_with_stages(
                        &child_def_for_query,
                        &child_display,
                        1,
                        HashMap::new(),
                        stage_ids,
                        context,
                    )
                    .await
                    .context(format!("failed to execute include '{include_name}'"))?;

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
                        stage_ids,
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

    /// Clone a query with a new ID.
    pub async fn clone_query(&self, source_id: &str, new_id: &str) -> Result<GatherQuery> {
        let source = self
            .queries
            .get(source_id)
            .ok_or_else(|| anyhow::anyhow!("query not found: {source_id}"))?
            .clone();

        let cloned = GatherQuery {
            query_id: new_id.to_string(),
            label: format!("{} (copy)", source.label),
            description: source.description.clone(),
            definition: source.definition.clone(),
            display: source.display.clone(),
            plugin: "admin".to_string(),
            created: 0, // will be set by register_query
            changed: 0,
        };

        self.register_query(cloned.clone()).await?;
        Ok(cloned)
    }

    /// Delete a query.
    pub async fn delete_query(&self, query_id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM gather_query WHERE query_id = $1")
            .bind(query_id)
            .execute(&self.pool)
            .await
            .context("failed to delete query")?;

        self.queries.remove(query_id);

        Ok(result.rows_affected() > 0)
    }

    /// Register core default gather queries.
    ///
    /// These provide standard queries that can replace hardcoded SQL
    /// throughout the admin interface and front-end.
    pub async fn register_default_views(&self) -> Result<()> {
        use super::types::{DisplayFormat, PagerConfig, PagerStyle, QuerySort, SortDirection};

        let defaults = vec![
            // ── 23.7: Core Content Gather Views ──
            GatherQuery {
                query_id: "core.published_items".to_string(),
                label: "Published items".to_string(),
                description: Some("All published content items".to_string()),
                definition: QueryDefinition {
                    base_table: "item".to_string(),
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
                    ..Default::default()
                },
                display: QueryDisplay {
                    format: DisplayFormat::Table,
                    items_per_page: 25,
                    pager: PagerConfig {
                        enabled: true,
                        style: PagerStyle::Full,
                        show_count: true,
                    },
                    empty_text: Some("No published content.".to_string()),
                    ..Default::default()
                },
                plugin: "core".to_string(),
                ..Default::default()
            },
            GatherQuery {
                query_id: "core.items_by_type".to_string(),
                label: "Items by type".to_string(),
                description: Some("Published items filtered by content type".to_string()),
                definition: QueryDefinition {
                    base_table: "item".to_string(),
                    filters: vec![
                        QueryFilter {
                            field: "status".to_string(),
                            operator: FilterOperator::Equals,
                            value: FilterValue::Integer(1),
                            exposed: false,
                            exposed_label: None,
                        },
                        QueryFilter {
                            field: "type".to_string(),
                            operator: FilterOperator::Equals,
                            value: FilterValue::String(String::new()),
                            exposed: true,
                            exposed_label: Some("Content type".to_string()),
                        },
                    ],
                    sorts: vec![QuerySort {
                        field: "created".to_string(),
                        direction: SortDirection::Desc,
                        nulls: None,
                    }],
                    ..Default::default()
                },
                display: QueryDisplay {
                    format: DisplayFormat::Table,
                    items_per_page: 25,
                    pager: PagerConfig {
                        enabled: true,
                        style: PagerStyle::Full,
                        show_count: true,
                    },
                    empty_text: Some("No items of this type.".to_string()),
                    ..Default::default()
                },
                plugin: "core".to_string(),
                ..Default::default()
            },
            GatherQuery {
                query_id: "core.items_by_author".to_string(),
                label: "Items by author".to_string(),
                description: Some("Published items by a specific author".to_string()),
                definition: QueryDefinition {
                    base_table: "item".to_string(),
                    filters: vec![
                        QueryFilter {
                            field: "status".to_string(),
                            operator: FilterOperator::Equals,
                            value: FilterValue::Integer(1),
                            exposed: false,
                            exposed_label: None,
                        },
                        QueryFilter {
                            field: "author_id".to_string(),
                            operator: FilterOperator::Equals,
                            value: FilterValue::Contextual(ContextualValue::CurrentUser),
                            exposed: false,
                            exposed_label: None,
                        },
                    ],
                    sorts: vec![QuerySort {
                        field: "created".to_string(),
                        direction: SortDirection::Desc,
                        nulls: None,
                    }],
                    ..Default::default()
                },
                display: QueryDisplay {
                    format: DisplayFormat::Table,
                    items_per_page: 25,
                    empty_text: Some("No items by this author.".to_string()),
                    ..Default::default()
                },
                plugin: "core".to_string(),
                ..Default::default()
            },
            GatherQuery {
                query_id: "core.all_items".to_string(),
                label: "All items".to_string(),
                description: Some("All content items (any status)".to_string()),
                definition: QueryDefinition {
                    base_table: "item".to_string(),
                    sorts: vec![QuerySort {
                        field: "changed".to_string(),
                        direction: SortDirection::Desc,
                        nulls: None,
                    }],
                    ..Default::default()
                },
                display: QueryDisplay {
                    format: DisplayFormat::Table,
                    items_per_page: 50,
                    pager: PagerConfig {
                        enabled: true,
                        style: PagerStyle::Full,
                        show_count: true,
                    },
                    empty_text: Some("No content items.".to_string()),
                    ..Default::default()
                },
                plugin: "core".to_string(),
                ..Default::default()
            },
            // ── 23.8: Admin Entity Gather Views ──
            GatherQuery {
                query_id: "core.user_list".to_string(),
                label: "Users".to_string(),
                description: Some("All user accounts".to_string()),
                definition: QueryDefinition {
                    base_table: "users".to_string(),
                    stage_aware: false,
                    filters: vec![QueryFilter {
                        field: "name".to_string(),
                        operator: FilterOperator::Contains,
                        value: FilterValue::String(String::new()),
                        exposed: true,
                        exposed_label: Some("Name".to_string()),
                    }],
                    sorts: vec![QuerySort {
                        field: "created".to_string(),
                        direction: SortDirection::Desc,
                        nulls: None,
                    }],
                    ..Default::default()
                },
                display: QueryDisplay {
                    format: DisplayFormat::Table,
                    items_per_page: 50,
                    empty_text: Some("No users found.".to_string()),
                    ..Default::default()
                },
                plugin: "core".to_string(),
                ..Default::default()
            },
            GatherQuery {
                query_id: "core.comment_list".to_string(),
                label: "Comments".to_string(),
                description: Some("All comments".to_string()),
                definition: QueryDefinition {
                    base_table: "comment".to_string(),
                    stage_aware: false,
                    sorts: vec![QuerySort {
                        field: "created".to_string(),
                        direction: SortDirection::Desc,
                        nulls: None,
                    }],
                    ..Default::default()
                },
                display: QueryDisplay {
                    format: DisplayFormat::Table,
                    items_per_page: 50,
                    empty_text: Some("No comments.".to_string()),
                    ..Default::default()
                },
                plugin: "core".to_string(),
                ..Default::default()
            },
            GatherQuery {
                query_id: "core.url_aliases".to_string(),
                label: "URL aliases".to_string(),
                description: Some("All URL aliases".to_string()),
                definition: QueryDefinition {
                    base_table: "url_alias".to_string(),
                    stage_aware: false,
                    filters: vec![QueryFilter {
                        field: "alias".to_string(),
                        operator: FilterOperator::Contains,
                        value: FilterValue::String(String::new()),
                        exposed: true,
                        exposed_label: Some("Path".to_string()),
                    }],
                    sorts: vec![QuerySort {
                        field: "alias".to_string(),
                        direction: SortDirection::Asc,
                        nulls: None,
                    }],
                    ..Default::default()
                },
                display: QueryDisplay {
                    format: DisplayFormat::Table,
                    items_per_page: 50,
                    empty_text: Some("No URL aliases.".to_string()),
                    ..Default::default()
                },
                plugin: "core".to_string(),
                ..Default::default()
            },
            GatherQuery {
                query_id: "core.roles".to_string(),
                label: "Roles".to_string(),
                description: Some("All user roles".to_string()),
                definition: QueryDefinition {
                    base_table: "role".to_string(),
                    stage_aware: false,
                    sorts: vec![QuerySort {
                        field: "weight".to_string(),
                        direction: SortDirection::Asc,
                        nulls: None,
                    }],
                    ..Default::default()
                },
                display: QueryDisplay {
                    format: DisplayFormat::Table,
                    items_per_page: 50,
                    empty_text: Some("No roles defined.".to_string()),
                    ..Default::default()
                },
                plugin: "core".to_string(),
                ..Default::default()
            },
            GatherQuery {
                query_id: "core.content_types".to_string(),
                label: "Content types".to_string(),
                description: Some("All content type definitions".to_string()),
                definition: QueryDefinition {
                    base_table: "content_type".to_string(),
                    stage_aware: false,
                    sorts: vec![QuerySort {
                        field: "label".to_string(),
                        direction: SortDirection::Asc,
                        nulls: None,
                    }],
                    ..Default::default()
                },
                display: QueryDisplay {
                    format: DisplayFormat::Table,
                    items_per_page: 50,
                    empty_text: Some("No content types defined.".to_string()),
                    ..Default::default()
                },
                plugin: "core".to_string(),
                ..Default::default()
            },
        ];

        for query in defaults {
            let query_id = query.query_id.clone();
            // Only register if not already in the database (don't overwrite customizations)
            if self.queries.get(&query_id).is_none() {
                self.register_query(query)
                    .await
                    .context(format!("failed to register default view '{query_id}'"))?;
            }
        }

        Ok(())
    }

    /// Validate a query definition for safety and correctness.
    ///
    /// Returns a list of validation errors. Empty list means valid.
    pub fn validate_definition(definition: &QueryDefinition) -> Vec<String> {
        let mut errors = Vec::new();

        // Max join depth
        const MAX_JOIN_DEPTH: usize = 3;
        if definition.relationships.len() > MAX_JOIN_DEPTH {
            errors.push(format!(
                "Too many relationships: {} (maximum {})",
                definition.relationships.len(),
                MAX_JOIN_DEPTH
            ));
        }

        // Base table must be non-empty
        if definition.base_table.is_empty() {
            errors.push("Base table is required".to_string());
        }

        // Validate base table name (alphanumeric + underscore only)
        if !definition
            .base_table
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_')
        {
            errors.push("Base table name contains invalid characters".to_string());
        }

        // Validate relationship table names and field names
        for rel in &definition.relationships {
            if !rel
                .target_table
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_')
            {
                errors.push(format!(
                    "Relationship target table '{}' contains invalid characters",
                    rel.target_table
                ));
            }
            if !is_valid_field_name(&rel.local_field) {
                errors.push(format!(
                    "Relationship local field '{}' contains invalid characters",
                    rel.local_field
                ));
            }
            if !is_valid_field_name(&rel.foreign_field) {
                errors.push(format!(
                    "Relationship foreign field '{}' contains invalid characters",
                    rel.foreign_field
                ));
            }
        }

        // Validate filter field names
        for filter in &definition.filters {
            if !is_valid_field_name(&filter.field) {
                errors.push(format!(
                    "Filter field '{}' contains invalid characters",
                    filter.field
                ));
            }
        }

        // Validate sort field names
        for sort in &definition.sorts {
            if !is_valid_field_name(&sort.field) {
                errors.push(format!(
                    "Sort field '{}' contains invalid characters",
                    sort.field
                ));
            }
        }

        errors
    }
}

/// Validate a field name for use in queries.
///
/// Allows alphanumeric, underscores, and dots (for JSONB paths like `fields.body`).
/// Must be non-empty and start with a letter or underscore.
fn is_valid_field_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // SAFETY: non-empty string confirmed by is_empty() check above
    let first = name.chars().next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

/// Maximum items per page (enforced by performance guardrails).
pub const MAX_ITEMS_PER_PAGE: u32 = 100;

/// Extract a string value from a JSON item by field path.
///
/// Handles top-level fields (`"id"`), single-level JSONB paths (`"fields.story_id"`),
/// and nested JSONB paths (`"fields.nested.deep"`). Returns `None` for null or
/// missing values to prevent false matches.
pub fn extract_field_value(item: &serde_json::Value, field_path: &str) -> Option<String> {
    if let Some(jsonb_path) = field_path.strip_prefix("fields.") {
        // JSONB path — the row_to_json result has a "fields" key with a JSON object
        // strip "fields."
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
    use crate::gather::types::{PagerConfig, PagerStyle, QuerySort, SortDirection};

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
    fn gather_query_serialization() {
        let gq = GatherQuery {
            query_id: "recent_articles".to_string(),
            label: "Recent Articles".to_string(),
            description: Some("Shows recent blog posts".to_string()),
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

        let json = serde_json::to_string(&gq).unwrap();
        let parsed: GatherQuery = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.query_id, "recent_articles");
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

        let def = QueryDefinition {
            filters: vec![QueryFilter {
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
            other => panic!("expected Uuid, got {other:?}"),
        }
    }

    #[test]
    fn resolve_contextual_current_user_anonymous() {
        let context = QueryContext::default();

        let def = QueryDefinition {
            filters: vec![QueryFilter {
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
            other => panic!("expected nil Uuid, got {other:?}"),
        }
    }

    #[test]
    fn resolve_contextual_current_time() {
        let context = QueryContext::default();
        let before = chrono::Utc::now().timestamp();

        let def = QueryDefinition {
            filters: vec![QueryFilter {
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
            other => panic!("expected Integer, got {other:?}"),
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

        let def = QueryDefinition {
            filters: vec![QueryFilter {
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
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn validate_definition_valid() {
        let def = QueryDefinition::default();
        let errors = GatherService::validate_definition(&def);
        assert!(
            errors.is_empty(),
            "default definition should be valid: {errors:?}"
        );
    }

    #[test]
    fn validate_definition_empty_base_table() {
        let def = QueryDefinition {
            base_table: "".to_string(),
            ..Default::default()
        };
        let errors = GatherService::validate_definition(&def);
        assert!(errors.iter().any(|e| e.contains("Base table is required")));
    }

    #[test]
    fn validate_definition_invalid_table_name() {
        let def = QueryDefinition {
            base_table: "item; DROP TABLE users".to_string(),
            ..Default::default()
        };
        let errors = GatherService::validate_definition(&def);
        assert!(errors.iter().any(|e| e.contains("invalid characters")));
    }

    #[test]
    fn validate_definition_too_many_joins() {
        use crate::gather::types::{JoinType, QueryRelationship};
        let def = QueryDefinition {
            relationships: (0..4)
                .map(|i| QueryRelationship {
                    name: format!("rel_{i}"),
                    target_table: format!("table_{i}"),
                    join_type: JoinType::Inner,
                    local_field: "id".to_string(),
                    foreign_field: "fk_id".to_string(),
                })
                .collect(),
            ..Default::default()
        };
        let errors = GatherService::validate_definition(&def);
        assert!(errors.iter().any(|e| e.contains("Too many relationships")));
    }

    #[test]
    fn validate_definition_invalid_relationship_table() {
        use crate::gather::types::{JoinType, QueryRelationship};
        let def = QueryDefinition {
            relationships: vec![QueryRelationship {
                name: "bad_rel".to_string(),
                target_table: "bad table!".to_string(),
                join_type: JoinType::Inner,
                local_field: "id".to_string(),
                foreign_field: "fk_id".to_string(),
            }],
            ..Default::default()
        };
        let errors = GatherService::validate_definition(&def);
        assert!(errors.iter().any(|e| e.contains("invalid characters")));
    }

    #[test]
    fn max_items_per_page_constant() {
        assert_eq!(MAX_ITEMS_PER_PAGE, 100);
    }

    #[test]
    fn is_valid_field_name_basic() {
        assert!(super::is_valid_field_name("status"));
        assert!(super::is_valid_field_name("created"));
        assert!(super::is_valid_field_name("fields.body"));
        assert!(super::is_valid_field_name("_internal"));
        assert!(super::is_valid_field_name("search_vector"));
    }

    #[test]
    fn is_valid_field_name_rejects_invalid() {
        assert!(!super::is_valid_field_name(""));
        assert!(!super::is_valid_field_name("1bad"));
        assert!(!super::is_valid_field_name("field; DROP TABLE"));
        assert!(!super::is_valid_field_name("field'name"));
        assert!(!super::is_valid_field_name("field name"));
    }

    #[test]
    fn validate_definition_invalid_filter_field() {
        let def = QueryDefinition {
            filters: vec![QueryFilter {
                field: "status; DROP TABLE".to_string(),
                operator: FilterOperator::Equals,
                value: FilterValue::Integer(1),
                exposed: false,
                exposed_label: None,
            }],
            ..Default::default()
        };
        let errors = GatherService::validate_definition(&def);
        assert!(
            errors.iter().any(|e| e.contains("Filter field")),
            "should reject invalid filter field: {errors:?}"
        );
    }

    #[test]
    fn validate_definition_invalid_sort_field() {
        let def = QueryDefinition {
            sorts: vec![QuerySort {
                field: "bad field!".to_string(),
                direction: SortDirection::Asc,
                nulls: None,
            }],
            ..Default::default()
        };
        let errors = GatherService::validate_definition(&def);
        assert!(
            errors.iter().any(|e| e.contains("Sort field")),
            "should reject invalid sort field: {errors:?}"
        );
    }
}
