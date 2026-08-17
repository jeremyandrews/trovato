//! Gather service for executing queries.
//!
//! Provides high-level query execution with:
//! - Query registration and lookup
//! - Category hierarchy resolution
//! - Exposed filter handling
//! - Result caching

use super::access;
use super::access::GatherAccessConfig;
use super::category_service::CategoryService;
use super::extension::GatherExtensionRegistry;
use super::query_builder::GatherQueryBuilder;
use super::types::{
    ContextualValue, FilterOperator, FilterValue, GatherQuery, GatherResult, QueryContext,
    QueryDefinition, QueryDisplay, QueryFilter,
};
use crate::content::{ItemService, RecordTypeRegistry};
use crate::services::ai_provider::AiProviderService;
use crate::services::vector_store::{PgVectorStore, VectorStore};
use crate::tap::UserContext;
use anyhow::{Context, Result};
use moka::sync::Cache;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use uuid::Uuid;

/// Maximum nesting depth for Gather relationship includes.
///
/// Prevents unbounded JOIN chains from plugins writing deep relationship
/// traversals. Depth 0 = no relationships, depth 1 = direct, depth 2 = nested,
/// depth 3 = maximum. Queries exceeding this are truncated silently.
///
/// This is the hard limit. Individual queries can specify a lower `max_depth`
/// in their definition. The `GATHER_MAX_RELATIONSHIP_DEPTH` env var could
/// override this in the future.
const MAX_INCLUDE_DEPTH: u8 = 3;

/// Maximum entries in the gather query cache.
const MAX_CAPACITY: u64 = 1_000;

/// TTL for the distinct-values cache (5 minutes).
const DISTINCT_VALUES_TTL: Duration = Duration::from_secs(300);

/// Maximum entries in the distinct-values cache.
const DISTINCT_VALUES_CAPACITY: u64 = 500;

// The semantic candidate pool size is `GatherAccessConfig::semantic_search_max` (Story 3.4 /
// D-26 over-fetch): the top-N nearest embeddings become the `id IN (...)`
// candidate set that the gather's own filters/sorts/pagination — and the
// access pass — compose over. No hard cosine-distance cutoff is applied
// (thresholds are model-specific and easy to misconfigure, PF-4 sub-decision 2).

/// Service for executing Gather queries.
pub struct GatherService {
    pool: PgPool,
    categories: Arc<CategoryService>,
    extensions: Arc<GatherExtensionRegistry>,
    /// Registered queries by query_id (TTL-bounded).
    queries: Cache<String, GatherQuery>,
    /// Cache of distinct field values per `"item_type::source_field"`.
    distinct_values_cache: Cache<String, Vec<String>>,
    /// Maximum per_page for query execution (from `GATHER_MAX_PAGE_SIZE`).
    max_page_size: u32,
    /// D-26 over-fetch and backfill bounds for access-filtered pages. Was a set
    /// of process-wide `LazyLock` statics reading the environment on first use;
    /// now an input, so two services in one process can differ.
    access: GatherAccessConfig,
    /// Optional embedding provider used to resolve `SemanticSimilarity`
    /// filters. `None` in builds without an AI provider wired (e.g. tests).
    ai_providers: Option<Arc<AiProviderService>>,
    /// Optional pgvector store backing semantic search.
    ///
    /// Typed as the concrete `PgVectorStore` rather than `Arc<dyn VectorStore>`
    /// because the [`VectorStore`] trait uses `async fn` in trait and is **not**
    /// object-safe (see its `async_fn_in_trait` allow). The trait is still the
    /// swap seam — only its methods are called here — and the D-OPEN
    /// `tap_vector_store` backend-registration path will pick a dyn-compatible
    /// shape when it lands.
    vector_store: Option<Arc<PgVectorStore>>,
    /// The shared item-access seam (Story 3.4), late-bound because `ItemService`
    /// is constructed after `GatherService` in `AppState`. Set once via
    /// [`Self::set_item_service`]. When unset (a narrow test harness that wires
    /// no item service), gather runs without the post-fetch access pass.
    item_access: OnceLock<Arc<ItemService>>,
    /// The lightweight-record type registry (P11g / D-54), late-bound like
    /// [`Self::item_access`]. When unset or a gather names no `record_type`, every
    /// gather is an Item gather and this is never consulted.
    record_types: OnceLock<Arc<RecordTypeRegistry>>,
}

/// Resolved context for a lightweight-record gather (P11g / D-54, D-55): the
/// record type name the FR-8 field-access seam dispatches under, the declared
/// published-flag column driving record-level visibility, and the logical-field
/// → physical-target map used to drop denied fields from result rows.
struct RecordContext {
    name: String,
    published_column: Option<String>,
    field_targets: HashMap<String, String>,
}

/// The settings [`GatherService`] runs with.
///
/// Grouped rather than passed as three more positional arguments: collaborators
/// stay explicit in [`GatherService::new`], while values that merely come from
/// configuration travel together. `access` used to arrive as process-wide
/// `LazyLock` statics that read the environment on first use, which is what this
/// struct replaces.
#[derive(Debug, Clone, Copy)]
pub struct GatherConfig {
    /// Registered-query cache TTL.
    pub ttl: Duration,
    /// Maximum `per_page` a request may ask for (`GATHER_MAX_PAGE_SIZE`).
    pub max_page_size: u32,
    /// D-26 over-fetch and backfill bounds for access-filtered pages.
    pub access: GatherAccessConfig,
}

impl GatherService {
    /// Create a new GatherService.
    ///
    /// `ai_providers` and `vector_store` back the `SemanticSimilarity` gather
    /// operator (embed query → pgvector search). Pass `None` for both in builds
    /// without semantic search; semantic filters then degrade to no-match.
    pub fn new(
        pool: PgPool,
        categories: Arc<CategoryService>,
        extensions: Arc<GatherExtensionRegistry>,
        config: GatherConfig,
        ai_providers: Option<Arc<AiProviderService>>,
        vector_store: Option<Arc<PgVectorStore>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            pool,
            categories,
            extensions,
            queries: Cache::builder()
                .max_capacity(MAX_CAPACITY)
                .time_to_live(config.ttl)
                .build(),
            distinct_values_cache: Cache::builder()
                .max_capacity(DISTINCT_VALUES_CAPACITY)
                .time_to_live(DISTINCT_VALUES_TTL)
                .build(),
            max_page_size: config.max_page_size,
            access: config.access,
            ai_providers,
            vector_store,
            item_access: OnceLock::new(),
            record_types: OnceLock::new(),
        })
    }

    /// Late-bind the shared item-access seam (Story 3.4). Called once by
    /// `AppState::new` after `ItemService` is constructed. A second call is a
    /// no-op (the first binding wins), so wiring is idempotent.
    pub fn set_item_service(&self, items: Arc<ItemService>) {
        let _ = self.item_access.set(items);
    }

    /// Late-bind the lightweight-record type registry (P11g / D-54). Called once
    /// by `AppState::new` after the registry is built. Idempotent (first wins).
    pub fn set_record_types(&self, record_types: Arc<RecordTypeRegistry>) {
        let _ = self.record_types.set(record_types);
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
        self.queries.get(query_id)
    }

    /// List all registered queries.
    pub fn list_queries(&self) -> Vec<GatherQuery> {
        self.queries.iter().map(|(_k, v)| v).collect()
    }

    /// Maximum number of distinct values returned by [`Self::fetch_distinct_values`].
    const DISTINCT_VALUES_LIMIT: i64 = 200;

    /// Fetch distinct non-empty values for a field within an item type.
    ///
    /// Only JSONB fields (path prefix `"fields."`) are supported. Returns an
    /// empty list for unrecognised or unsafe field names.
    ///
    /// Results are capped at `DISTINCT_VALUES_LIMIT` and cached for
    /// `DISTINCT_VALUES_TTL` (5 min) per `(item_type, source_field)` pair.
    ///
    /// **Stage note:** This query filters by `status = 1` but does not filter
    /// by `stage_id`. Widget options therefore reflect published-status items
    /// across all stages. This is intentional: widgets show the universe of
    /// possible values; the query itself applies stage filtering to results.
    pub async fn fetch_distinct_values(
        &self,
        source_field: &str,
        item_type: &str,
    ) -> Result<Vec<String>> {
        if item_type.is_empty() {
            tracing::debug!(
                source_field,
                "dynamic_options widget has no item_type; \
                 distinct values query skipped (would return nothing)"
            );
            return Ok(Vec::new());
        }

        let cache_key = format!("{item_type}::{source_field}");

        if let Some(cached) = self.distinct_values_cache.get(&cache_key) {
            return Ok(cached);
        }

        // Only JSONB paths (e.g. "fields.field_country") are supported.
        let Some(jsonb_key) = source_field.strip_prefix("fields.") else {
            return Ok(Vec::new());
        };

        // Validate the key portion so it is safe to use as a bind parameter.
        if !is_valid_field_name(jsonb_key) {
            return Ok(Vec::new());
        }

        let values: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT fields->>$1 \
             FROM item \
             WHERE type = $2 \
               AND status = 1 \
               AND fields->>$1 IS NOT NULL \
               AND fields->>$1 <> '' \
             ORDER BY 1 \
             LIMIT $3",
        )
        .bind(jsonb_key)
        .bind(item_type)
        .bind(Self::DISTINCT_VALUES_LIMIT)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch distinct field values")?;

        self.distinct_values_cache.insert(cache_key, values.clone());

        Ok(values)
    }

    /// Build scope conditions for faceted-option queries.
    ///
    /// Walks `all_exposed` and, for each filter whose field differs from
    /// `exclude_field` and has an active value in `active`, produces either:
    ///
    /// - An equality condition `(jsonb_key, string_value)` for `Equals` filters.
    /// - A tag-membership condition `(jsonb_key, uuid_strings)` for
    ///   `HasTagOrDescendants` filters (the tag hierarchy is expanded here).
    ///
    /// Returns `(equals_scope, tag_scope)`.
    async fn build_facet_scope(
        &self,
        all_exposed: &[QueryFilter],
        exclude_field: &str,
        active: &HashMap<String, FilterValue>,
    ) -> Result<(Vec<(String, String)>, Vec<(String, Vec<String>)>)> {
        let mut eq_scope: Vec<(String, String)> = Vec::new();
        let mut tag_scope: Vec<(String, Vec<String>)> = Vec::new();

        for filter in all_exposed {
            if !filter.exposed || filter.field == exclude_field {
                continue;
            }
            let Some(active_value) = active.get(&filter.field) else {
                continue;
            };
            // Only JSONB paths (e.g. "fields.field_country") are supported.
            let Some(jsonb_key) = filter.field.strip_prefix("fields.") else {
                continue;
            };
            if !is_valid_field_name(jsonb_key) {
                continue;
            }
            match filter.operator {
                FilterOperator::Equals => {
                    if let Some(s) = active_value.as_string() {
                        eq_scope.push((jsonb_key.to_string(), s));
                    }
                }
                FilterOperator::HasTagOrDescendants => {
                    if let Some(tag_id) = active_value.as_uuid() {
                        let ids = self.categories.get_tag_with_descendants(tag_id).await?;
                        let id_strings: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
                        if !id_strings.is_empty() {
                            tag_scope.push((jsonb_key.to_string(), id_strings));
                        }
                    }
                }
                _ => {
                    // Other operators (GreaterOrEqual, IsNotNull, etc.) are not
                    // used as exposed filters in current schemas; skip them.
                }
            }
        }

        Ok((eq_scope, tag_scope))
    }

    /// Fetch distinct values for `source_field` constrained by other active filters.
    ///
    /// Falls back to the cached [`Self::fetch_distinct_values`] when no scope conditions
    /// apply (initial page load with no active selections). Runs a live uncached
    /// query when other filters are active so option lists stay consistent with
    /// the current result set.
    pub async fn fetch_faceted_distinct_values(
        &self,
        source_field: &str,
        item_type: &str,
        all_exposed: &[QueryFilter],
        exclude_field: &str,
        active: &HashMap<String, FilterValue>,
    ) -> Result<Vec<String>> {
        let (eq_scope, tag_scope) = self
            .build_facet_scope(all_exposed, exclude_field, active)
            .await?;

        if eq_scope.is_empty() && tag_scope.is_empty() {
            return self.fetch_distinct_values(source_field, item_type).await;
        }

        if item_type.is_empty() {
            return Ok(Vec::new());
        }

        let Some(jsonb_key) = source_field.strip_prefix("fields.") else {
            return Ok(Vec::new());
        };
        if !is_valid_field_name(jsonb_key) {
            return Ok(Vec::new());
        }

        // Build SQL with validated scope conditions interpolated as key names.
        let mut sql = "SELECT DISTINCT fields->>$1 \
             FROM item \
             WHERE type = $2 \
               AND status = 1 \
               AND fields->>$1 IS NOT NULL \
               AND fields->>$1 <> ''"
            .to_string();
        let mut param_idx = 3i32;
        for (key, _) in &eq_scope {
            // key validated by is_valid_field_name above
            sql.push_str(&format!(" AND fields->>'{key}' = ${param_idx}"));
            param_idx += 1;
        }
        for (key, _) in &tag_scope {
            // key validated by is_valid_field_name above
            sql.push_str(&format!(
                " AND EXISTS (\
                    SELECT 1 FROM jsonb_array_elements_text(fields->'{key}') t \
                    WHERE t = ANY(${param_idx}))"
            ));
            param_idx += 1;
        }
        sql.push_str(&format!(
            " ORDER BY 1 LIMIT {}",
            Self::DISTINCT_VALUES_LIMIT
        ));

        let mut q = sqlx::query_scalar::<_, String>(&sql)
            .bind(jsonb_key)
            .bind(item_type);
        for (_, val) in &eq_scope {
            q = q.bind(val.clone());
        }
        for (_, ids) in &tag_scope {
            q = q.bind(ids.clone());
        }

        q.fetch_all(&self.pool)
            .await
            .context("failed to fetch faceted distinct values")
    }

    /// Fetch UUIDs of tags appearing in `tag_field` for items matching the scope.
    ///
    /// Returns `None` when no scope conditions are active, signalling the caller
    /// to show the full category tree without filtering. Returns `Some(set)` (possibly
    /// empty) when scope conditions are active.
    pub async fn fetch_faceted_reachable_tag_ids(
        &self,
        tag_field: &str,
        item_type: &str,
        all_exposed: &[QueryFilter],
        exclude_field: &str,
        active: &HashMap<String, FilterValue>,
    ) -> Result<Option<HashSet<Uuid>>> {
        let (eq_scope, tag_scope) = self
            .build_facet_scope(all_exposed, exclude_field, active)
            .await?;

        if eq_scope.is_empty() && tag_scope.is_empty() {
            return Ok(None);
        }

        if item_type.is_empty() {
            return Ok(Some(HashSet::new()));
        }

        let Some(jsonb_key) = tag_field.strip_prefix("fields.") else {
            return Ok(None);
        };
        if !is_valid_field_name(jsonb_key) {
            return Ok(None);
        }

        // key validated by is_valid_field_name above
        let mut sql = format!(
            "SELECT DISTINCT t.value \
             FROM item \
             CROSS JOIN LATERAL jsonb_array_elements_text(fields->'{jsonb_key}') AS t(value) \
             WHERE type = $1 \
               AND status = 1"
        );
        let mut param_idx = 2i32;
        for (key, _) in &eq_scope {
            // key validated by is_valid_field_name above
            sql.push_str(&format!(" AND fields->>'{key}' = ${param_idx}"));
            param_idx += 1;
        }
        for (key, _) in &tag_scope {
            // key validated by is_valid_field_name above
            sql.push_str(&format!(
                " AND EXISTS (\
                    SELECT 1 FROM jsonb_array_elements_text(fields->'{key}') tt \
                    WHERE tt = ANY(${param_idx}))"
            ));
            param_idx += 1;
        }

        let mut q = sqlx::query_scalar::<_, String>(&sql).bind(item_type);
        for (_, val) in &eq_scope {
            q = q.bind(val.clone());
        }
        for (_, ids) in &tag_scope {
            q = q.bind(ids.clone());
        }

        let tag_strings: Vec<String> = q
            .fetch_all(&self.pool)
            .await
            .context("failed to fetch reachable tag IDs")?;

        let tag_ids: HashSet<Uuid> = tag_strings
            .iter()
            .filter_map(|s| Uuid::parse_str(s).ok())
            .collect();

        Ok(Some(tag_ids))
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

    /// Reload all queries from the database into cache.
    ///
    /// Called periodically by the background reload task to keep the
    /// cache fresh so that external database changes become visible.
    /// Delegates to [`load_queries`](Self::load_queries), then ensures
    /// redirects exist for queries with a `canonical_url`.
    pub async fn reload_from_db(&self) -> Result<()> {
        self.load_queries().await?;
        self.sync_canonical_redirects().await;
        Ok(())
    }

    /// Ensure 301 redirects exist from `/gather/{query_id}` to `canonical_url`
    /// for every query that has one set.
    ///
    /// Idempotent — uses `ON CONFLICT DO NOTHING` so existing or manually
    /// edited redirects are preserved. Silently skips if the `redirect`
    /// table does not exist (plugin not installed).
    async fn sync_canonical_redirects(&self) {
        for (_key, query) in &self.queries {
            let Some(ref canonical) = query.display.canonical_url else {
                continue;
            };
            let source = format!("/gather/{}", query.query_id);
            let result = sqlx::query(
                "INSERT INTO redirect (id, source, destination, status_code, language, created) \
                 SELECT gen_random_uuid(), $1, $2, 301, 'en', EXTRACT(EPOCH FROM NOW())::bigint \
                 WHERE NOT EXISTS ( \
                     SELECT 1 FROM redirect WHERE source = $1 AND language = 'en' \
                 )",
            )
            .bind(&source)
            .bind(canonical)
            .execute(&self.pool)
            .await;

            if let Err(e) = result {
                let msg = e.to_string();
                // Ignore "relation does not exist" — redirects plugin not installed
                if !msg.contains("redirect") || !msg.contains("does not exist") {
                    tracing::warn!(
                        query_id = %query.query_id,
                        error = %e,
                        "failed to sync canonical redirect"
                    );
                }
            }
        }
    }

    /// Execute a registered query by ID.
    pub async fn execute(
        &self,
        query_id: &str,
        page: u32,
        exposed_filters: HashMap<String, FilterValue>,
        stage_id: Uuid,
        context: &QueryContext,
    ) -> Result<GatherResult> {
        self.execute_with_stages(query_id, page, exposed_filters, &[stage_id], context)
            .await
    }

    /// Execute a registered query with stage overlay.
    ///
    /// Items in any of the provided stages will be included in results.
    pub async fn execute_with_stages(
        &self,
        query_id: &str,
        page: u32,
        exposed_filters: HashMap<String, FilterValue>,
        stage_ids: &[Uuid],
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
        stage_id: Uuid,
        context: &QueryContext,
    ) -> Result<GatherResult> {
        self.execute_definition_with_stages(
            definition,
            display,
            page,
            exposed_filters,
            &[stage_id],
            context,
        )
        .await
    }

    /// Execute a query definition with stage overlay.
    pub async fn execute_definition_with_stages(
        &self,
        definition: &QueryDefinition,
        display: &QueryDisplay,
        page: u32,
        exposed_filters: HashMap<String, FilterValue>,
        stage_ids: &[Uuid],
        context: &QueryContext,
    ) -> Result<GatherResult> {
        // Performance guardrails: validate definition before execution
        let validation_errors = Self::validate_definition(definition);
        if !validation_errors.is_empty() {
            anyhow::bail!("Query validation failed: {}", validation_errors.join("; "));
        }

        // Cap items_per_page to the configured maximum (GATHER_MAX_PAGE_SIZE).
        let max_page = self.max_page_size;
        let resolved_display = if display.items_per_page > max_page {
            let requested = display.items_per_page;
            tracing::warn!(
                requested = requested,
                capped = max_page,
                "items_per_page exceeds maximum, capping"
            );
            let mut capped = display.clone();
            capped.items_per_page = max_page;
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
        let mut resolved_definition = Self::resolve_contextual_values(resolved_definition, context);

        // Resolve a lightweight-record gather (P11g / D-54): rewrite the
        // definition to target the record's table with translated field
        // references, and capture the record context (name + published column +
        // field targets) the builder and field-access pass need. `None` for every
        // Item gather, leaving its definition — and emitted SQL — untouched.
        let record_ctx = self.resolve_record_context(&mut resolved_definition)?;

        // Resolve category hierarchy for HasTagOrDescendants filters
        let resolved_definition = self
            .resolve_category_hierarchies(resolved_definition)
            .await?;

        // Resolve custom filter extensions (expand hierarchies, etc.)
        let final_definition = self.resolve_custom_filters(resolved_definition).await?;

        // Resolve semantic-similarity filters: embed the query text, run a
        // pgvector search, and rewrite to an `id IN (...)` candidate set. Runs
        // last so the rewritten predicate composes with every other resolved
        // filter, sort, and the pager.
        let final_definition = self.resolve_semantic_filters(final_definition).await?;

        // Split includes from definition to avoid cloning the full tree
        // just for the query builder (which only uses filters/sorts/fields).
        let includes = final_definition.includes.clone();
        // Field-projection metadata for the field-access pass (Story 3.4),
        // captured before `final_definition` is moved into the builder def.
        let is_star = final_definition.fields.is_empty();
        let field_map = access::field_projection_map(&final_definition.fields);
        let output_keys: Vec<String> = final_definition
            .fields
            .iter()
            .map(|f| match f.field_name.strip_prefix("fields.") {
                Some(path) => f.label.clone().unwrap_or_else(|| path.to_string()),
                None => f.label.clone().unwrap_or_else(|| f.field_name.clone()),
            })
            .collect();
        let builder_def = QueryDefinition {
            includes: HashMap::new(),
            ..final_definition
        };

        // Build and execute queries (per_page already clamped in resolved_display above)
        let per_page = display.items_per_page;
        let builder = GatherQueryBuilder::new_with_stages(builder_def, stage_ids.to_vec())
            .with_extensions(self.extensions.clone())
            .with_language(context.language.clone())
            .with_viewer(context.viewer.clone())
            .with_record_published(record_ctx.as_ref().and_then(|c| c.published_column.clone()));

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

        // Count reflects the SQL-expressible access predicate (D-26 note: it is
        // not the exact access-filtered total — the residual item/plugin tier is
        // enforced post-fetch and is not counted; access-aware totals are not a
        // 1.0 goal).
        let count_sql = builder.build_count();
        let total: i64 = sqlx::query_scalar(&count_sql)
            .fetch_one(&mut *tx)
            .await
            .context("failed to execute count query")?;

        // Item-access + over-fetch/backfill (Story 3.4, §4 / D-26) then
        // field-level filtering across the page.
        let viewer = context
            .viewer
            .clone()
            .unwrap_or_else(UserContext::anonymous);
        let (mut rows, access_capped) = if let Some(rc) = &record_ctx {
            // Lightweight-record page (P11g / D-54, D-55): the record-level
            // (published) filter is already an exact SQL predicate, so there is no
            // per-item over-fetch loop — fetch the page and refine fields through
            // the same FR-8 seam Items use.
            self.fetch_record_page(&builder, &viewer, page, per_page, &mut tx, rc)
                .await?
        } else {
            self.fetch_access_filtered(
                &builder,
                &viewer,
                page,
                per_page,
                &mut tx,
                is_star,
                &field_map,
                &output_keys,
            )
            .await?
        };

        // Commit transaction (SET LOCAL resets automatically)
        tx.commit()
            .await
            .context("failed to commit query transaction")?;

        // Execute includes (batched sub-queries). Child gathers inherit the
        // viewer via the cloned context, so they are access-filtered too.
        if !includes.is_empty() {
            self.execute_includes(&mut rows, &includes, stage_ids, context, 0)
                .await?;
        }

        Ok(GatherResult::new(rows, total as u64, page, per_page).with_access_capped(access_capped))
    }

    /// Item-access enforcement with the D-26 over-fetch/geometric-backfill loop.
    ///
    /// Fetches candidate rows in rank order starting at the page's raw offset and
    /// runs the authoritative `check_access` pass over them, backfilling with
    /// geometrically-growing windows when the page underfills, until the page is
    /// full, the source is exhausted, or the hard scan cap is hit. Returns the
    /// visible rows (field-filtered) and the access-capped signal. When the
    /// item-access seam is unwired (a narrow test harness), it degrades to a
    /// single unfiltered window.
    #[allow(clippy::too_many_arguments)] // Threaded query state; a struct would only obscure it.
    async fn fetch_access_filtered(
        &self,
        builder: &GatherQueryBuilder,
        viewer: &UserContext,
        page: u32,
        per_page: u32,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        is_star: bool,
        field_map: &[(String, String)],
        output_keys: &[String],
    ) -> Result<(Vec<serde_json::Value>, bool)> {
        let Some(items) = self.item_access.get() else {
            // Unwired test harness: no enforcement, single window.
            let main_sql = builder.build(page, per_page);
            let rows = sqlx::query_scalar(&format!("SELECT row_to_json(t) FROM ({main_sql}) t"))
                .fetch_all(&mut **tx)
                .await
                .context("failed to execute main query")?;
            return Ok((rows, false));
        };

        let page_size = per_page.max(1) as usize;
        let base_offset = (page.saturating_sub(1) as u64) * per_page as u64;
        let max_scan = u64::from(self.access.max_scan);
        let max_rounds = self.access.max_backfill_rounds;
        let mut window = u64::from(per_page.max(1)) * u64::from(self.access.fetch_factor);

        let mut visible: Vec<serde_json::Value> = Vec::with_capacity(page_size);
        let mut scanned: u64 = 0;
        let mut round: u32 = 0;
        let mut exhausted = false;

        while visible.len() < page_size && scanned < max_scan && round < max_rounds {
            round += 1;
            // Clamp so the cumulative scan never exceeds the hard cap.
            let this_window = window.min(max_scan - scanned);
            let main_sql = builder.build_window(base_offset + scanned, this_window);
            let batch: Vec<serde_json::Value> =
                sqlx::query_scalar(&format!("SELECT row_to_json(t) FROM ({main_sql}) t"))
                    .fetch_all(&mut **tx)
                    .await
                    .context("failed to execute main query window")?;
            let got = batch.len() as u64;
            scanned += got;

            for row in batch {
                if visible.len() >= page_size {
                    break;
                }
                // A row we cannot reconstruct cannot be access-checked → drop
                // (deny). The authoritative check_access catches plugin denies
                // and the authenticated role tier the SQL predicate omits.
                match access::access_item_from_row(&row) {
                    Some(item)
                        if items
                            .check_access(&item, "view", viewer)
                            .await
                            .unwrap_or(false) =>
                    {
                        visible.push(row);
                    }
                    _ => {}
                }
            }

            if got < this_window {
                exhausted = true;
                break;
            }
            window = window.saturating_mul(2);
        }

        // Capped iff the page never filled and we stopped on the scan/round cap
        // rather than exhausting the candidate source.
        let access_capped = visible.len() < page_size && !exhausted;

        self.apply_field_filtering(items, viewer, &mut visible, is_star, field_map, output_keys)
            .await;

        Ok((visible, access_capped))
    }

    /// Tier-2 field-level filtering over the visible page (Story 3.4 AC-2).
    ///
    /// One `field_access_decisions` dispatch per **distinct type** (N+1-free).
    /// For `SELECT item.*` rows, denied keys are dropped from the row's `fields`
    /// object. For explicit-field gathers, each row is rebuilt to only its
    /// originally-requested output keys minus denied dynamic fields — which also
    /// strips the access columns injected for the item-access pass.
    async fn apply_field_filtering(
        &self,
        items: &ItemService,
        viewer: &UserContext,
        rows: &mut [serde_json::Value],
        is_star: bool,
        field_map: &[(String, String)],
        output_keys: &[String],
    ) {
        if rows.is_empty() {
            return;
        }

        // Gather the field names to decide per type.
        let mut fields_by_type: HashMap<String, HashSet<String>> = HashMap::new();
        for row in rows.iter() {
            let Some(item_type) = row.get("type").and_then(|v| v.as_str()) else {
                continue;
            };
            let entry = fields_by_type.entry(item_type.to_string()).or_default();
            if is_star {
                if let Some(obj) = row.get("fields").and_then(|v| v.as_object()) {
                    for k in obj.keys() {
                        entry.insert(k.clone());
                    }
                }
            } else {
                for (_out, item_field) in field_map {
                    entry.insert(item_field.clone());
                }
            }
        }

        // One dispatch per type; a `false` decision means "drop this field".
        let mut decisions_by_type: HashMap<String, HashMap<String, bool>> = HashMap::new();
        for (item_type, names) in fields_by_type {
            let names: Vec<String> = names.into_iter().collect();
            let decisions = items
                .field_access_decisions(viewer, &item_type, &names, "view")
                .await;
            decisions_by_type.insert(item_type, decisions);
        }

        for row in rows.iter_mut() {
            let item_type = row
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let decisions = decisions_by_type.get(&item_type);
            access::filter_row_fields(row, is_star, field_map, output_keys, decisions);
        }
    }

    /// Resolve a lightweight-record gather (P11g / D-54).
    ///
    /// When `definition.record_type` names a registered record type, rewrite the
    /// definition to target the record's table — `base_table` set, `item_type`
    /// cleared (records have no `type` column), `stage_aware` cleared (no
    /// `stage_id`) — and translate every filter/sort/projection **logical** field
    /// name through the record's field map to its physical column or JSONB path.
    /// Returns the [`RecordContext`] the builder (published predicate) and the
    /// field-access pass need. Returns `Ok(None)` for an Item gather (definition
    /// untouched). Errors if a `record_type` is named but not registered — a
    /// misconfigured gather must fail closed, not silently query the wrong table.
    fn resolve_record_context(
        &self,
        definition: &mut QueryDefinition,
    ) -> Result<Option<RecordContext>> {
        let Some(record_name) = definition.record_type.clone() else {
            return Ok(None);
        };
        let def = self
            .record_types
            .get()
            .and_then(|registry| registry.get(&record_name));
        let Some(def) = def else {
            anyhow::bail!("gather references unknown lightweight-record type '{record_name}'");
        };

        definition.base_table = def.table.clone();
        definition.item_type = None;
        definition.stage_aware = false;

        for filter in &mut definition.filters {
            if let Some(target) = def.resolve_field(&filter.field) {
                filter.field = target.to_string();
            }
        }
        for sort in &mut definition.sorts {
            if let Some(target) = def.resolve_field(&sort.field) {
                sort.field = target.to_string();
            }
        }
        for field in &mut definition.fields {
            if let Some(target) = def.resolve_field(&field.field_name) {
                field.field_name = target.to_string();
            }
        }

        Ok(Some(RecordContext {
            name: record_name,
            published_column: def.published_column.clone(),
            field_targets: def.field_map.clone(),
        }))
    }

    /// Fetch one page of a lightweight-record gather (P11g / D-54, D-55).
    ///
    /// Record-level visibility (the declared published flag) is already an exact
    /// SQL predicate in the builder, so — unlike the Item path — there is no
    /// per-row `check_access` over-fetch loop (records have no per-row access tap
    /// in 1.0). Fetch the window, then refine **field** visibility through the
    /// same FR-8 seam Items use, keyed on the record type name.
    async fn fetch_record_page(
        &self,
        builder: &GatherQueryBuilder,
        viewer: &UserContext,
        page: u32,
        per_page: u32,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        record_ctx: &RecordContext,
    ) -> Result<(Vec<serde_json::Value>, bool)> {
        let offset = (page.saturating_sub(1) as u64) * u64::from(per_page);
        let main_sql = builder.build_window(offset, u64::from(per_page.max(1)));
        let mut rows: Vec<serde_json::Value> =
            sqlx::query_scalar(&format!("SELECT row_to_json(t) FROM ({main_sql}) t"))
                .fetch_all(&mut **tx)
                .await
                .context("failed to execute record query window")?;

        if let Some(items) = self.item_access.get() {
            Self::apply_record_field_filtering(items, viewer, &mut rows, record_ctx).await;
        }

        // Record-level visibility is the exact SQL predicate, so a short page
        // means the source is exhausted, never an access cap.
        Ok((rows, false))
    }

    /// Field-level access for a lightweight-record page (P11g / D-55).
    ///
    /// Routes through the **same** `field_access_decisions` core the Item seam
    /// uses — one batched `tap_field_access` dispatch keyed on the record type
    /// name over the record's declared logical field names (the field-map keys,
    /// the record analog of Item dynamic fields). A denied logical field has its
    /// physical target (a top-level column or a `fields.`-rooted JSONB key)
    /// removed from every row. Deny-wins and the fail-open default are identical
    /// to Items: there is no second access path.
    async fn apply_record_field_filtering(
        items: &ItemService,
        viewer: &UserContext,
        rows: &mut [serde_json::Value],
        record_ctx: &RecordContext,
    ) {
        if rows.is_empty() || record_ctx.field_targets.is_empty() {
            return;
        }
        let logical: Vec<String> = record_ctx.field_targets.keys().cloned().collect();
        let decisions = items
            .field_access_decisions(viewer, &record_ctx.name, &logical, "view")
            .await;

        for row in rows.iter_mut() {
            for (logical_name, target) in &record_ctx.field_targets {
                if !decisions.get(logical_name).copied().unwrap_or(true) {
                    remove_record_field(row, target);
                }
            }
        }
    }

    /// Apply exposed filter values from user input, then **drop** any exposed
    /// filter left without one.
    ///
    /// An exposed filter is a question put to the reader, and a reader who does
    /// not answer it is asking for everything — so an unanswered exposed filter
    /// must place no constraint at all. It used to keep its *definition* value
    /// instead, which for the common `""` default made `Equals` emit
    /// `field = ''`: an empty list over an Item gather's JSONB text, and over a
    /// record type's real `uuid` column a hard `invalid input syntax for type
    /// uuid: ""` — a 500 on the page's own default state
    /// (**G-EXPOSED-FILTER-NO-MATCH-ALL**, Argus M3; M1's shipped `/articles`
    /// route was serving exactly that).
    ///
    /// `In`, `NotIn` and `FullTextSearch` already skipped empty values in the
    /// query builder, and `HasTagOrDescendants` already dropped non-UUIDs in
    /// [`Self::resolve_category_hierarchies`]; this makes the rule uniform
    /// across every operator rather than true of four of them.
    ///
    /// `IsNull` / `IsNotNull` are exempt: they are complete without a value, so
    /// "no value" is not the unanswered state for them.
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

        definition.filters.retain(|filter| {
            let unanswered = Self::exposed_filter_is_unanswered(filter);
            if unanswered {
                tracing::debug!(
                    field = %filter.field,
                    "exposed filter left unanswered; dropping it (match-all)"
                );
            }
            !unanswered
        });

        Ok(definition)
    }

    /// Whether a filter is an exposed one the reader left unanswered, and so
    /// must place no constraint. See [`Self::resolve_exposed_filters`].
    fn exposed_filter_is_unanswered(filter: &QueryFilter) -> bool {
        filter.exposed
            && !matches!(
                filter.operator,
                FilterOperator::IsNull | FilterOperator::IsNotNull
            )
            && filter.value.is_unset()
    }

    /// Resolve category hierarchy filters by expanding tag IDs.
    ///
    /// Filters with a non-UUID value (e.g. exposed filters whose user has not
    /// yet provided a value, or contextual `UrlArg` filters when the argument
    /// is absent) are silently **dropped** rather than causing an error. This
    /// makes `HasTagOrDescendants` behave like other optional exposed filters:
    /// no value → no constraint → all results are returned.
    async fn resolve_category_hierarchies(
        &self,
        mut definition: QueryDefinition,
    ) -> Result<QueryDefinition> {
        let mut resolved_filters = Vec::new();

        for filter in definition.filters {
            if filter.operator == FilterOperator::HasTagOrDescendants {
                // Skip if no UUID value is present (exposed filter with no
                // user input, or UrlArg not found in context).
                let Some(tag_id) = filter.value.as_uuid() else {
                    continue;
                };

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
                    widget: Default::default(),
                });
            } else {
                resolved_filters.push(filter);
            }
        }

        definition.filters = resolved_filters;
        Ok(definition)
    }

    /// Resolve `SemanticSimilarity` filters into a concrete `id IN (...)`
    /// predicate the query builder can express.
    ///
    /// For each semantic filter: embed its text value via the configured
    /// embedding provider, run a pgvector `similarity_search`, and rewrite the
    /// filter to `id In [matched item ids]` (the ranked candidate set). The
    /// rest of the gather definition then composes over those candidates.
    ///
    /// The ranked id order is also recorded on `QueryDefinition.relevance_order`
    /// so the query builder can return results most-similar-first when the
    /// gather defines no explicit `sorts`. An explicit gather sort overrides it
    /// (see [`QueryDefinition::relevance_order`] and `add_sorts`).
    ///
    /// Degradation — any of: no AI/vector wiring, pgvector unavailable, no
    /// embedding provider configured, empty query text, an embedding failure,
    /// or zero matches — leaves the `SemanticSimilarity` filter *in place*. The
    /// query builder's `SemanticSimilarity` arm then yields `FALSE` (no-match),
    /// its documented safety-net role. This path never errors the surrounding
    /// gather.
    async fn resolve_semantic_filters(
        &self,
        mut definition: QueryDefinition,
    ) -> Result<QueryDefinition> {
        // Fast path: nothing to embed if no semantic filters are present.
        if !definition
            .filters
            .iter()
            .any(|f| f.operator == FilterOperator::SemanticSimilarity)
        {
            return Ok(definition);
        }

        let mut resolved_filters = Vec::with_capacity(definition.filters.len());
        // The ranked candidate order from the first semantic filter that
        // produces matches becomes the gather's relevance order. With a single
        // semantic filter (the common case) this is unambiguous; if a gather
        // ever combines two, the first one's ranking wins rather than an
        // arbitrary interleave. Carried to the query builder so unsorted
        // semantic gathers return most-similar-first.
        let mut relevance_order: Option<Vec<Uuid>> = None;
        for filter in definition.filters {
            if filter.operator != FilterOperator::SemanticSimilarity {
                resolved_filters.push(filter);
                continue;
            }

            match self.semantic_candidate_ids(&filter).await {
                Some(ids) if !ids.is_empty() => {
                    if relevance_order.is_none() {
                        relevance_order = Some(ids.clone());
                    }
                    resolved_filters.push(QueryFilter {
                        field: "id".to_string(),
                        operator: FilterOperator::In,
                        value: FilterValue::List(ids.into_iter().map(FilterValue::Uuid).collect()),
                        exposed: filter.exposed,
                        exposed_label: filter.exposed_label,
                        widget: Default::default(),
                    });
                }
                // No candidates (degradation or zero matches): keep the
                // semantic filter so the builder produces FALSE (no-match).
                // An empty `In` list would instead *drop* the filter and widen
                // the result set — exactly the wrong outcome.
                _ => resolved_filters.push(filter),
            }
        }

        definition.filters = resolved_filters;
        definition.relevance_order = relevance_order;
        Ok(definition)
    }

    /// Embed a semantic filter's query text and return the ranked candidate
    /// item ids from pgvector, or `None` on any degradation path.
    ///
    /// `Some(vec)` (possibly empty) means the search ran; `None` means the
    /// search could not run (no wiring, unavailable, no provider, empty text,
    /// or an error) and the caller must fall back to no-match.
    async fn semantic_candidate_ids(&self, filter: &QueryFilter) -> Option<Vec<Uuid>> {
        let (Some(ai), Some(store)) = (&self.ai_providers, &self.vector_store) else {
            return None;
        };

        // pgvector unavailable → graceful no-match (preserves prior behavior).
        if !store.is_available().await {
            return None;
        }

        let query_text = filter.value.as_string()?;
        let query_text = query_text.trim();
        if query_text.is_empty() {
            return None;
        }

        let embedding = match ai.embed(query_text).await {
            Ok(Some(result)) => result,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(error = %e, "semantic filter embedding failed; returning no-match");
                return None;
            }
        };

        // Over-fetch the candidate pool (Story 3.4 / D-26): access filtering runs
        // after this cap, so a restricted viewer needs a deeper pool than the
        // historical top-100 to avoid starvation. Bounded by `semantic_search_max`.
        match store
            .similarity_search(
                &embedding.vector,
                &embedding.model,
                self.access.semantic_search_max,
            )
            .await
        {
            Ok(results) => Some(results.into_iter().map(|r| r.item_id).collect()),
            Err(e) => {
                tracing::warn!(error = %e, "semantic similarity_search failed; returning no-match");
                None
            }
        }
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
                    ContextualValue::CurrentDate => {
                        FilterValue::String(chrono::Local::now().format("%Y-%m-%d").to_string())
                    }
                    // An absent URL argument is the contextual plane's version
                    // of an unanswered exposed filter, and used to resolve to
                    // `""` — which `Equals` emitted as `field = ''`, erroring
                    // outright over a `uuid` column (G-EXPOSED-FILTER-NO-MATCH-ALL,
                    // the same defect reached from `/stories/topic` with no
                    // `?topic=`). `Null` carries no constraint for every
                    // operator, matching `HasTagOrDescendants`' documented
                    // "no value → no constraint".
                    //
                    // Deliberately *not* extended to `CurrentUser`: an anonymous
                    // viewer resolves to the nil uuid and must keep constraining,
                    // or a reader-scoped gather would return every row.
                    ContextualValue::UrlArg(name) => context
                        .url_args
                        .get(name)
                        .filter(|v| !v.trim().is_empty())
                        .map(|v| FilterValue::String(v.clone()))
                        .unwrap_or(FilterValue::Null(())),
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
        stage_ids: &'a [Uuid],
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
                    widget: Default::default(),
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

        self.queries.invalidate(query_id);

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
                        widget: Default::default(),
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
                            widget: Default::default(),
                        },
                        QueryFilter {
                            field: "type".to_string(),
                            operator: FilterOperator::Equals,
                            value: FilterValue::String(String::new()),
                            exposed: true,
                            exposed_label: Some("Content type".to_string()),
                            widget: Default::default(),
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
                            widget: Default::default(),
                        },
                        QueryFilter {
                            field: "author_id".to_string(),
                            operator: FilterOperator::Equals,
                            value: FilterValue::Contextual(ContextualValue::CurrentUser),
                            exposed: false,
                            exposed_label: None,
                            widget: Default::default(),
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
                        widget: Default::default(),
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
                        widget: Default::default(),
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

        // Validate base table name (ASCII alphanumeric + underscore, must start
        // with letter or underscore, max 63 chars — matching is_safe_identifier)
        if !is_safe_table_name(&definition.base_table) {
            errors.push("Base table name contains invalid characters".to_string());
        }

        // Validate relationship table names, aliases, and field names
        for rel in &definition.relationships {
            if !is_safe_table_name(&rel.target_table) {
                errors.push(format!(
                    "Relationship target table '{}' contains invalid characters",
                    rel.target_table
                ));
            }
            if !is_safe_table_name(&rel.name) {
                errors.push(format!(
                    "Relationship alias '{}' contains invalid characters",
                    rel.name
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

        // Validate select field names and table aliases
        for field in &definition.fields {
            if !is_valid_field_name(&field.field_name) {
                errors.push(format!(
                    "Select field '{}' contains invalid characters",
                    field.field_name
                ));
            }
            if let Some(ref alias) = field.table_alias
                && !is_safe_table_name(alias)
            {
                errors.push(format!(
                    "Select field table alias '{alias}' contains invalid characters"
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

/// Validate a table/alias name for use in queries.
///
/// ASCII alphanumeric + underscore only, must start with a letter or
/// underscore, max 63 chars (PostgreSQL identifier limit). Mirrors
/// `is_safe_identifier()` in `handlers.rs` for defense-in-depth at the
/// service-validation layer.
fn is_safe_table_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Validate a field name for use in queries.
///
/// Allows alphanumeric, underscores, and dots (for JSONB paths like `fields.body`).
/// Must be non-empty and start with a letter or underscore.
fn is_valid_field_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // Infallible: non-empty string confirmed by is_empty() check above
    #[allow(clippy::unwrap_used)]
    let first = name.chars().next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

/// Remove a lightweight-record field's physical target from a `row_to_json`
/// result row (P11g / D-55), used when the FR-8 field seam denies the field.
///
/// A plain column target (`"location"`) drops the top-level key. A
/// `fields.`-rooted target (`"fields.capacity"`, nested `"fields.a.b"`) descends
/// the row's `fields` object and drops the leaf key. No-op if the row or the
/// intermediate path is not an object — a denied field that is simply absent from
/// the row needs no removal.
fn remove_record_field(row: &mut serde_json::Value, target: &str) {
    let Some(obj) = row.as_object_mut() else {
        return;
    };
    match target.split_once('.') {
        // Plain column: drop the top-level key.
        None => {
            obj.remove(target);
        }
        // `fields.<path>`: descend the `fields` object to the leaf and drop it.
        Some((root, rest)) => {
            let Some(mut cursor) = obj.get_mut(root).and_then(|v| v.as_object_mut()) else {
                return;
            };
            let mut segments = rest.split('.').peekable();
            while let Some(segment) = segments.next() {
                if segments.peek().is_none() {
                    cursor.remove(segment);
                    return;
                }
                let Some(next) = cursor.get_mut(segment).and_then(|v| v.as_object_mut()) else {
                    return;
                };
                cursor = next;
            }
        }
    }
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
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Tests marked `SECURITY REGRESSION TEST` verify fixes for specific security
    //! findings from Epic 27. Do not remove without security review.

    use super::*;
    use crate::gather::types::{PagerConfig, PagerStyle, QuerySort, SortDirection};

    #[test]
    fn remove_record_field_drops_plain_column() {
        let mut row = serde_json::json!({"id": "x", "location": "Barga", "name": "Conf"});
        remove_record_field(&mut row, "location");
        assert!(row.get("location").is_none());
        assert!(row.get("name").is_some(), "other columns untouched");
    }

    #[test]
    fn remove_record_field_drops_jsonb_leaf() {
        let mut row = serde_json::json!({
            "id": "x",
            "fields": {"capacity": 500, "keep": "yes"}
        });
        remove_record_field(&mut row, "fields.capacity");
        let fields = row.get("fields").and_then(|v| v.as_object()).unwrap();
        assert!(!fields.contains_key("capacity"));
        assert!(fields.contains_key("keep"), "sibling JSONB keys untouched");
    }

    #[test]
    fn remove_record_field_noop_when_absent_or_not_object() {
        // Absent target: no panic, no change.
        let mut row = serde_json::json!({"id": "x"});
        remove_record_field(&mut row, "location");
        remove_record_field(&mut row, "fields.capacity");
        assert_eq!(row, serde_json::json!({"id": "x"}));
        // Non-object row: no-op.
        let mut scalar = serde_json::json!("not-an-object");
        remove_record_field(&mut scalar, "location");
        assert_eq!(scalar, serde_json::json!("not-an-object"));
    }

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
            plugin: "trovato_blog".to_string(),
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
            viewer: None,
            url_args: HashMap::new(),
            language: None,
        };

        let def = QueryDefinition {
            filters: vec![QueryFilter {
                field: "fields.user_id".to_string(),
                operator: FilterOperator::Equals,
                value: FilterValue::Contextual(ContextualValue::CurrentUser),
                exposed: false,
                exposed_label: None,
                widget: Default::default(),
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
                widget: Default::default(),
            }],
            ..Default::default()
        };

        let resolved = GatherService::resolve_contextual_values(def, &context);
        match &resolved.filters[0].value {
            FilterValue::Uuid(u) => assert_eq!(*u, Uuid::nil()),
            other => panic!("expected nil Uuid, got {other:?}"),
        }
    }

    // =======================================================================
    // G-EXPOSED-FILTER-NO-MATCH-ALL (Argus M3): an unanswered exposed filter
    // must match all, not nothing — and must not reach Postgres as `= ''`.
    // =======================================================================

    fn filter(field: &str, op: FilterOperator, value: FilterValue, exposed: bool) -> QueryFilter {
        QueryFilter {
            field: field.to_string(),
            operator: op,
            value,
            exposed,
            exposed_label: None,
            widget: Default::default(),
        }
    }

    #[test]
    fn an_unanswered_exposed_filter_is_dropped_for_every_operator() {
        // The shape M1's `argus_article_list` shipped: an exposed `equals`
        // filter defaulting to `""`. Over a record type's real `uuid` column
        // that used to raise `invalid input syntax for type uuid: ""`.
        for op in [
            FilterOperator::Equals,
            FilterOperator::NotEquals,
            FilterOperator::Contains,
            FilterOperator::StartsWith,
            FilterOperator::EndsWith,
            FilterOperator::GreaterThan,
            FilterOperator::LessThan,
            FilterOperator::In,
            FilterOperator::FullTextSearch,
        ] {
            assert!(
                GatherService::exposed_filter_is_unanswered(&filter(
                    "topic_id",
                    op.clone(),
                    FilterValue::String(String::new()),
                    true,
                )),
                "{op:?} with a blank exposed value must place no constraint"
            );
        }
    }

    #[test]
    fn an_answered_exposed_filter_still_constrains() {
        assert!(!GatherService::exposed_filter_is_unanswered(&filter(
            "topic_id",
            FilterOperator::Equals,
            FilterValue::String("a-topic".into()),
            true,
        )));
    }

    #[test]
    fn a_non_exposed_filter_is_never_dropped() {
        // A definition-authored filter means what it says, including `= ''`.
        assert!(!GatherService::exposed_filter_is_unanswered(&filter(
            "fields.slug",
            FilterOperator::Equals,
            FilterValue::String(String::new()),
            false,
        )));
    }

    #[test]
    fn a_valueless_operator_survives_being_exposed_and_unset() {
        for op in [FilterOperator::IsNull, FilterOperator::IsNotNull] {
            assert!(
                !GatherService::exposed_filter_is_unanswered(&filter(
                    "fields.retired_at",
                    op,
                    FilterValue::Null(()),
                    true,
                )),
                "IsNull/IsNotNull are complete without a value"
            );
        }
    }

    #[test]
    fn an_exposed_contextual_filter_survives_until_it_is_resolved() {
        // Dropped here it could never resolve; and once resolved, an anonymous
        // `CurrentUser` is the nil uuid, which must keep constraining.
        assert!(!GatherService::exposed_filter_is_unanswered(&filter(
            "fields.user_id",
            FilterOperator::Equals,
            FilterValue::Contextual(ContextualValue::CurrentUser),
            true,
        )));
        assert!(!GatherService::exposed_filter_is_unanswered(&filter(
            "fields.user_id",
            FilterOperator::Equals,
            FilterValue::Uuid(Uuid::nil()),
            true,
        )));
    }

    #[test]
    fn resolve_contextual_url_arg_absent_carries_no_constraint() {
        // `/stories/topic` with no `?topic=` is the contextual plane's version
        // of the same defect: it used to resolve to `""` and error over a uuid
        // column. `Null` is skipped by every operator in the query builder.
        let def = QueryDefinition {
            filters: vec![filter(
                "topic_id",
                FilterOperator::Equals,
                FilterValue::Contextual(ContextualValue::UrlArg("topic".into())),
                false,
            )],
            ..Default::default()
        };

        let resolved = GatherService::resolve_contextual_values(def, &QueryContext::default());
        assert!(
            resolved.filters[0].value.is_null(),
            "an absent url arg must not become an empty string, got {:?}",
            resolved.filters[0].value
        );
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
                widget: Default::default(),
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
            viewer: None,
            url_args,
            language: None,
        };

        let def = QueryDefinition {
            filters: vec![QueryFilter {
                field: "fields.category".to_string(),
                operator: FilterOperator::Equals,
                value: FilterValue::Contextual(ContextualValue::UrlArg("category".to_string())),
                exposed: false,
                exposed_label: None,
                widget: Default::default(),
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
    fn resolve_contextual_current_date() {
        let context = QueryContext::default();
        let before = chrono::Local::now().format("%Y-%m-%d").to_string();

        let def = QueryDefinition {
            filters: vec![QueryFilter {
                field: "fields.field_start_date".to_string(),
                operator: FilterOperator::GreaterOrEqual,
                value: FilterValue::Contextual(ContextualValue::CurrentDate),
                exposed: false,
                exposed_label: None,
                widget: Default::default(),
            }],
            ..Default::default()
        };

        let resolved = GatherService::resolve_contextual_values(def, &context);
        let after = chrono::Local::now().format("%Y-%m-%d").to_string();

        match &resolved.filters[0].value {
            FilterValue::String(s) => {
                assert!(
                    s >= &before && s <= &after,
                    "expected date between {before} and {after}, got {s}"
                );
            }
            other => panic!("expected String date, got {other:?}"),
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

    // SECURITY REGRESSION TEST — Story 27.2: SQL injection in table name rejected
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

    // SECURITY REGRESSION TEST — Story 27.2: invalid relationship table rejected
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

    // SECURITY REGRESSION TEST — Story 27.2: SQL injection in field names rejected
    #[test]
    fn is_valid_field_name_rejects_invalid() {
        assert!(!super::is_valid_field_name(""));
        assert!(!super::is_valid_field_name("1bad"));
        assert!(!super::is_valid_field_name("field; DROP TABLE"));
        assert!(!super::is_valid_field_name("field'name"));
        assert!(!super::is_valid_field_name("field name"));
    }

    // SECURITY REGRESSION TEST — Story 27.2: SQL injection in filter field rejected
    #[test]
    fn validate_definition_invalid_filter_field() {
        let def = QueryDefinition {
            filters: vec![QueryFilter {
                field: "status; DROP TABLE".to_string(),
                operator: FilterOperator::Equals,
                value: FilterValue::Integer(1),
                exposed: false,
                exposed_label: None,
                widget: Default::default(),
            }],
            ..Default::default()
        };
        let errors = GatherService::validate_definition(&def);
        assert!(
            errors.iter().any(|e| e.contains("Filter field")),
            "should reject invalid filter field: {errors:?}"
        );
    }

    // SECURITY REGRESSION TEST — Story 27.2: invalid sort field rejected
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

    // SECURITY REGRESSION TEST — Story 27.2: SQL injection in select field rejected
    #[test]
    fn validate_definition_invalid_select_field() {
        use crate::gather::types::QueryField;
        let def = QueryDefinition {
            fields: vec![QueryField {
                field_name: "fields.body'; DROP TABLE".to_string(),
                table_alias: None,
                label: None,
            }],
            ..Default::default()
        };
        let errors = GatherService::validate_definition(&def);
        assert!(
            errors.iter().any(|e| e.contains("Select field")),
            "should reject invalid select field: {errors:?}"
        );
    }

    // SECURITY REGRESSION TEST — Story 27.2: invalid table alias rejected
    #[test]
    fn validate_definition_invalid_table_alias() {
        use crate::gather::types::QueryField;
        let def = QueryDefinition {
            fields: vec![QueryField {
                field_name: "id".to_string(),
                table_alias: Some("bad table!".to_string()),
                label: None,
            }],
            ..Default::default()
        };
        let errors = GatherService::validate_definition(&def);
        assert!(
            errors.iter().any(|e| e.contains("table alias")),
            "should reject invalid table alias: {errors:?}"
        );
    }

    // SECURITY REGRESSION TEST — Story 27.2: Unicode table alias rejected (ASCII-only)
    #[test]
    fn validate_definition_unicode_table_alias_rejected() {
        use crate::gather::types::QueryField;
        let def = QueryDefinition {
            fields: vec![QueryField {
                field_name: "id".to_string(),
                table_alias: Some("café".to_string()),
                label: None,
            }],
            ..Default::default()
        };
        let errors = GatherService::validate_definition(&def);
        assert!(
            errors.iter().any(|e| e.contains("table alias")),
            "should reject unicode table alias: {errors:?}"
        );
    }

    // SECURITY REGRESSION TEST — Story 27.2: base table starting with digit rejected
    #[test]
    fn validate_definition_base_table_starts_with_digit() {
        let def = QueryDefinition {
            base_table: "123table".to_string(),
            ..Default::default()
        };
        let errors = GatherService::validate_definition(&def);
        assert!(
            errors.iter().any(|e| e.contains("Base table")),
            "should reject base table starting with digit: {errors:?}"
        );
    }

    // SECURITY REGRESSION TEST — Story 27.2: invalid relationship alias rejected
    #[test]
    fn validate_definition_invalid_rel_name() {
        use crate::gather::types::QueryRelationship;
        let def = QueryDefinition {
            relationships: vec![QueryRelationship {
                name: "bad alias!".to_string(),
                target_table: "users".to_string(),
                join_type: crate::gather::types::JoinType::Inner,
                local_field: "user_id".to_string(),
                foreign_field: "id".to_string(),
            }],
            ..Default::default()
        };
        let errors = GatherService::validate_definition(&def);
        assert!(
            errors.iter().any(|e| e.contains("Relationship alias")),
            "should reject invalid relationship name: {errors:?}"
        );
    }

    // SECURITY REGRESSION TEST — Story 27.2: table alias exceeding 63 chars rejected
    #[test]
    fn validate_definition_long_table_alias_rejected() {
        use crate::gather::types::QueryField;
        let def = QueryDefinition {
            fields: vec![QueryField {
                field_name: "id".to_string(),
                table_alias: Some("a".repeat(64)),
                label: None,
            }],
            ..Default::default()
        };
        let errors = GatherService::validate_definition(&def);
        assert!(
            errors.iter().any(|e| e.contains("table alias")),
            "should reject table alias exceeding 63 chars: {errors:?}"
        );
    }
}
