//! Gather query builder using SeaQuery.
//!
//! Generates type-safe SQL queries from QueryDefinition with support for:
//! - JSONB field extraction
//! - Category hierarchy filters
//! - Stage-aware queries
//! - Pagination

use super::extension::{FilterContext, GatherExtensionRegistry};
use super::handlers::is_safe_identifier;
use super::types::{
    FilterOperator, FilterValue, JoinType, QueryDefinition, QueryFilter, SortDirection,
};
use sea_query::{
    Alias, Asterisk, Cond, Expr, ExprTrait, Iden, Order, PostgresQueryBuilder, Query,
    SelectStatement, SimpleExpr,
};
use std::sync::Arc;

/// Identifier for dynamic table/column names.
#[allow(dead_code)]
#[derive(Iden)]
#[iden = "item"]
struct ItemTable;

/// Query builder for Gather queries.
pub struct GatherQueryBuilder {
    definition: QueryDefinition,
    stage_ids: Vec<String>,
    extensions: Option<Arc<GatherExtensionRegistry>>,
}

impl GatherQueryBuilder {
    /// Create a new query builder targeting a single stage.
    pub fn new(definition: QueryDefinition, stage_id: &str) -> Self {
        Self {
            definition,
            stage_ids: vec![stage_id.to_string()],
            extensions: None,
        }
    }

    /// Create a query builder with stage overlay (hierarchy).
    ///
    /// When multiple stages are provided (e.g., `["review", "draft", "live"]`),
    /// the query will match items in ANY of those stages, enabling content
    /// overlay where child stages inherit from parents.
    pub fn new_with_stages(definition: QueryDefinition, stage_ids: Vec<String>) -> Self {
        Self {
            definition,
            stage_ids,
            extensions: None,
        }
    }

    /// Set the extension registry for custom filter/sort/relationship handling.
    pub fn with_extensions(mut self, extensions: Arc<GatherExtensionRegistry>) -> Self {
        self.extensions = Some(extensions);
        self
    }

    /// Build the main SELECT query with pagination.
    pub fn build(&self, page: u32, per_page: u32) -> String {
        let mut query = Query::select();

        // SELECT fields
        self.add_select_fields(&mut query);

        // FROM base table
        query.from(Alias::new(&self.definition.base_table));

        // JOINs
        self.add_joins(&mut query);

        // WHERE conditions
        self.add_filters(&mut query);

        // Filter by stage (only for stage-aware tables like `item`)
        self.add_stage_filter(&mut query);

        // Filter by item_type if specified
        if let Some(ref item_type) = self.definition.item_type {
            query.and_where(
                Expr::col((Alias::new(&self.definition.base_table), Alias::new("type")))
                    .eq(item_type),
            );
        }

        // ORDER BY
        self.add_sorts(&mut query);

        // LIMIT/OFFSET for pagination
        let offset = ((page.saturating_sub(1)) * per_page) as u64;
        query.limit(per_page as u64);
        query.offset(offset);

        query.to_string(PostgresQueryBuilder)
    }

    /// Build a COUNT query for total results.
    pub fn build_count(&self) -> String {
        let mut query = Query::select();

        // SELECT COUNT(*)
        query.expr(Expr::col(Asterisk).count());

        // FROM base table
        query.from(Alias::new(&self.definition.base_table));

        // JOINs
        self.add_joins(&mut query);

        // WHERE conditions
        self.add_filters(&mut query);

        // Stage filter (only for stage-aware tables)
        self.add_stage_filter(&mut query);

        // Item type filter
        if let Some(ref item_type) = self.definition.item_type {
            query.and_where(
                Expr::col((Alias::new(&self.definition.base_table), Alias::new("type")))
                    .eq(item_type),
            );
        }

        query.to_string(PostgresQueryBuilder)
    }

    /// Add stage_id filter to the query if the definition is stage-aware.
    ///
    /// Uses `= $val` for a single stage, `IN (...)` for hierarchy overlay.
    fn add_stage_filter(&self, query: &mut SelectStatement) {
        if !self.definition.stage_aware {
            return;
        }
        let col = Expr::col((
            Alias::new(&self.definition.base_table),
            Alias::new("stage_id"),
        ));
        if self.stage_ids.len() == 1 {
            query.and_where(col.eq(&self.stage_ids[0]));
        } else {
            query.and_where(col.is_in(&self.stage_ids));
        }
    }

    /// Add SELECT fields to the query.
    fn add_select_fields(&self, query: &mut SelectStatement) {
        if self.definition.fields.is_empty() {
            // Select all from base table
            query.column((Alias::new(&self.definition.base_table), Asterisk));
        } else {
            for field in &self.definition.fields {
                let table = field
                    .table_alias
                    .as_deref()
                    .unwrap_or(&self.definition.base_table);

                if field.field_name.starts_with("fields.") {
                    // JSONB field extraction
                    let jsonb_path = &field.field_name[7..]; // Strip "fields."
                    let expr = self.jsonb_extract_expr(table, jsonb_path);
                    if let Some(ref label) = field.label {
                        query.expr_as(expr, Alias::new(label));
                    } else {
                        query.expr_as(expr, Alias::new(jsonb_path));
                    }
                } else {
                    // Regular column
                    query.column((Alias::new(table), Alias::new(&field.field_name)));
                }
            }
        }
    }

    /// Add JOIN clauses.
    fn add_joins(&self, query: &mut SelectStatement) {
        for rel in &self.definition.relationships {
            let join_type = match rel.join_type {
                JoinType::Inner => sea_query::JoinType::InnerJoin,
                JoinType::Left => sea_query::JoinType::LeftJoin,
                JoinType::Right => sea_query::JoinType::RightJoin,
            };

            let on_condition = Expr::col((
                Alias::new(&self.definition.base_table),
                Alias::new(&rel.local_field),
            ))
            .equals((Alias::new(&rel.name), Alias::new(&rel.foreign_field)));

            query.join_as(
                join_type,
                Alias::new(&rel.target_table),
                Alias::new(&rel.name),
                on_condition,
            );
        }
    }

    /// Add WHERE conditions from filters.
    fn add_filters(&self, query: &mut SelectStatement) {
        for filter in &self.definition.filters {
            if let Some(condition) = self.build_filter_condition(filter) {
                query.and_where(condition);
            }
        }
    }

    /// Build a single filter condition.
    fn build_filter_condition(&self, filter: &QueryFilter) -> Option<SimpleExpr> {
        let field_expr = self.field_expr(&filter.field);

        match &filter.operator {
            FilterOperator::Equals => {
                let value = filter.value.as_string()?;
                Some(field_expr.eq(value))
            }
            FilterOperator::NotEquals => {
                let value = filter.value.as_string()?;
                Some(field_expr.ne(value))
            }
            FilterOperator::Contains => {
                let value = filter.value.as_string()?;
                Some(field_expr.like(format!("%{}%", escape_like_wildcards(&value))))
            }
            FilterOperator::StartsWith => {
                let value = filter.value.as_string()?;
                Some(field_expr.like(format!("{}%", escape_like_wildcards(&value))))
            }
            FilterOperator::EndsWith => {
                let value = filter.value.as_string()?;
                Some(field_expr.like(format!("%{}", escape_like_wildcards(&value))))
            }
            FilterOperator::GreaterThan => {
                let value = filter.value.as_i64()?;
                Some(field_expr.gt(value))
            }
            FilterOperator::LessThan => {
                let value = filter.value.as_i64()?;
                Some(field_expr.lt(value))
            }
            FilterOperator::GreaterOrEqual => {
                let value = filter.value.as_i64()?;
                Some(field_expr.gte(value))
            }
            FilterOperator::LessOrEqual => {
                let value = filter.value.as_i64()?;
                Some(field_expr.lte(value))
            }
            FilterOperator::In => {
                let values = self.extract_string_list(&filter.value);
                if values.is_empty() {
                    return None;
                }
                Some(field_expr.is_in(values))
            }
            FilterOperator::NotIn => {
                let values = self.extract_string_list(&filter.value);
                if values.is_empty() {
                    return None;
                }
                Some(field_expr.is_not_in(values))
            }
            FilterOperator::IsNull => Some(field_expr.is_null()),
            FilterOperator::IsNotNull => Some(field_expr.is_not_null()),
            // Full-text search using PostgreSQL tsvector
            FilterOperator::FullTextSearch => {
                let value = filter.value.as_string()?;
                if value.is_empty() {
                    return None;
                }
                // Defense-in-depth: validate base_table before interpolation
                if !is_safe_identifier(&self.definition.base_table) {
                    tracing::error!(
                        base_table =
                            &self.definition.base_table[..self.definition.base_table.len().min(64)],
                        "unsafe base table in FTS expression; restricting results"
                    );
                    return Some(Expr::cust("FALSE"));
                }
                // Sanitize: keep only alphanumeric + spaces, then join with &
                let sanitized: String = value
                    .chars()
                    .map(|c| {
                        if c.is_alphanumeric() || c == ' ' {
                            c
                        } else {
                            ' '
                        }
                    })
                    .collect();
                let terms: Vec<&str> = sanitized.split_whitespace().collect();
                if terms.is_empty() {
                    return None;
                }
                let tsquery = terms.join(" & ");
                // Use parameterized query to prevent SQL injection
                Some(Expr::cust_with_values(
                    format!(
                        "{}.search_vector @@ to_tsquery('english', $1)",
                        self.definition.base_table
                    ),
                    [tsquery],
                ))
            }
            // Category operators - these need special handling with subqueries
            FilterOperator::HasTag => {
                let uuid = filter.value.as_uuid()?;
                self.build_category_filter(&filter.field, vec![uuid], false)
            }
            FilterOperator::HasAnyTag => {
                let uuids = filter.value.as_uuid_list();
                if uuids.is_empty() {
                    return None;
                }
                self.build_category_filter(&filter.field, uuids, false)
            }
            FilterOperator::HasAllTags => {
                // For "has all", we need AND conditions for each tag
                let uuids = filter.value.as_uuid_list();
                if uuids.is_empty() {
                    return None;
                }
                let mut cond = Cond::all();
                for uuid in uuids {
                    if let Some(expr) = self.build_category_filter(&filter.field, vec![uuid], false)
                    {
                        cond = cond.add(expr);
                    }
                }
                Some(cond.into())
            }
            FilterOperator::HasTagOrDescendants => {
                let uuid = filter.value.as_uuid()?;
                self.build_category_filter(&filter.field, vec![uuid], true)
            }
            FilterOperator::Custom(name) => {
                if let Some(ref extensions) = self.extensions
                    && let Some((handler, config)) = extensions.get_filter(name)
                {
                    let ctx = FilterContext {
                        base_table: self.definition.base_table.clone(),
                        stage_id: self.stage_ids.first().cloned().unwrap_or_default(),
                    };
                    match handler.build_condition(filter, config, &ctx) {
                        Ok(expr) => return expr,
                        Err(e) => {
                            tracing::error!(
                                filter = name,
                                error = %e,
                                "custom filter handler failed; restricting results"
                            );
                            // Return FALSE to restrict rather than widen query results
                            return Some(Expr::cust("FALSE"));
                        }
                    }
                }
                tracing::error!(
                    filter = name,
                    "custom filter operator has no registered extension; restricting results"
                );
                // Return FALSE to restrict rather than widen query results
                Some(Expr::cust("FALSE"))
            }
        }
    }

    /// Build expression for a field (handles JSONB paths).
    fn field_expr(&self, field: &str) -> SimpleExpr {
        if let Some(jsonb_path) = field.strip_prefix("fields.") {
            self.jsonb_extract_expr(&self.definition.base_table, jsonb_path)
        } else {
            Expr::col((Alias::new(&self.definition.base_table), Alias::new(field))).into()
        }
    }

    /// Extract a value from a JSONB column.
    ///
    /// Validates table and path components against SQL identifier rules
    /// (defense-in-depth). Returns NULL expression if validation fails.
    fn jsonb_extract_expr(&self, table: &str, path: &str) -> SimpleExpr {
        // Defense-in-depth: validate table name before interpolation
        if !is_safe_identifier(table) {
            tracing::error!(
                table = &table[..table.len().min(64)],
                "unsafe table name in JSONB expression; returning NULL"
            );
            return Expr::cust("NULL");
        }

        // Use ->> for text extraction from JSONB
        // e.g., fields->>'body' for fields.body
        if path.contains('.') {
            // Nested path: fields->'nested'->>'field'
            let parts: Vec<&str> = path.split('.').collect();
            for part in &parts {
                if !is_safe_identifier(part) {
                    tracing::error!(
                        path = &path[..path.len().min(64)],
                        "unsafe JSONB path component; returning NULL"
                    );
                    return Expr::cust("NULL");
                }
            }
            let mut expr = format!("{table}.fields");
            for (i, part) in parts.iter().enumerate() {
                if i == parts.len() - 1 {
                    expr = format!("({expr}->>'{part}')");
                } else {
                    expr = format!("({expr}->'{part}')");
                }
            }
            Expr::cust(expr)
        } else {
            if !is_safe_identifier(path) {
                tracing::error!(
                    path = &path[..path.len().min(64)],
                    "unsafe JSONB path; returning NULL"
                );
                return Expr::cust("NULL");
            }
            Expr::cust(format!("{table}.fields->>'{path}'"))
        }
    }

    /// Build a category filter condition.
    ///
    /// Uses `jsonb_extract_expr()` for validated JSONB path extraction and
    /// SeaQuery's `is_in()` for parameterized UUID values.
    ///
    /// **Note:** `include_descendants` is accepted but not yet implemented —
    /// `HasTagOrDescendants` currently behaves identically to `HasTag`.
    /// TODO: Implement recursive CTE expansion for descendant tag matching.
    fn build_category_filter(
        &self,
        field: &str,
        tag_ids: Vec<uuid::Uuid>,
        include_descendants: bool,
    ) -> Option<SimpleExpr> {
        if include_descendants {
            tracing::warn!(
                "include_descendants is not yet implemented; falling back to exact match"
            );
        }
        let jsonb_path = field.strip_prefix("fields.").unwrap_or(field);
        let jsonb_expr = self.jsonb_extract_expr(&self.definition.base_table, jsonb_path);
        let uuid_strings: Vec<String> = tag_ids.iter().map(|u| u.to_string()).collect();
        Some(jsonb_expr.is_in(uuid_strings))
    }

    /// Add ORDER BY clauses.
    fn add_sorts(&self, query: &mut SelectStatement) {
        for sort in &self.definition.sorts {
            let order = match sort.direction {
                SortDirection::Asc => Order::Asc,
                SortDirection::Desc => Order::Desc,
            };

            if sort.field.starts_with("fields.") {
                let jsonb_path = &sort.field[7..];
                let expr = self.jsonb_extract_expr(&self.definition.base_table, jsonb_path);
                // Note: NULLS FIRST/LAST not yet supported via SeaQuery API for expressions
                // TODO: Add support when SeaQuery adds this feature
                let _nulls = &sort.nulls;
                query.order_by_expr(expr, order);
            } else {
                query.order_by(
                    (
                        Alias::new(&self.definition.base_table),
                        Alias::new(&sort.field),
                    ),
                    order,
                );
            }
        }
    }

    /// Extract a list of strings from a FilterValue.
    fn extract_string_list(&self, value: &FilterValue) -> Vec<String> {
        match value {
            FilterValue::List(items) => items.iter().filter_map(|v| v.as_string()).collect(),
            FilterValue::String(s) => vec![s.clone()],
            FilterValue::Uuid(u) => vec![u.to_string()],
            _ => Vec::new(),
        }
    }
}

/// Escape SQL LIKE wildcard characters (`%`, `_`, `\`) in a value.
fn escape_like_wildcards(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Builder for creating category hierarchy subqueries.
#[allow(dead_code)]
pub struct CategoryHierarchyQuery;

#[allow(dead_code)]
impl CategoryHierarchyQuery {
    /// Build a recursive CTE to get a tag and all its descendants.
    /// Returns the SQL for the WITH clause that can be prepended to the main query.
    ///
    /// # Safety (SQL)
    ///
    /// `tag_id` is `uuid::Uuid` whose `Display` impl only produces hex digits
    /// and hyphens — safe for direct interpolation into SQL literals.
    pub fn descendants_cte(tag_id: uuid::Uuid) -> String {
        format!(
            r#"WITH RECURSIVE tag_descendants AS (
    SELECT '{tag_id}'::uuid as id
    UNION ALL
    SELECT h.tag_id
    FROM category_tag_hierarchy h
    INNER JOIN tag_descendants d ON h.parent_id = d.id
)"#
        )
    }

    /// Build a filter expression that checks if a JSONB field is in the descendants CTE.
    ///
    /// Validates `field_path` against SQL identifier rules (defense-in-depth).
    /// Returns `FALSE` expression if validation fails.
    pub fn in_descendants_expr(field_path: &str) -> SimpleExpr {
        if !is_safe_identifier(field_path) {
            tracing::error!(
                field_path = &field_path[..field_path.len().min(64)],
                "unsafe field path in descendants expression"
            );
            return Expr::cust("FALSE");
        }
        Expr::cust(format!(
            "(fields->>'{field_path}')::uuid IN (SELECT id FROM tag_descendants)"
        ))
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::gather::types::{QueryField, QueryFilter, QuerySort};

    #[test]
    fn simple_query_build() {
        let def = QueryDefinition {
            base_table: "item".to_string(),
            item_type: Some("blog".to_string()),
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
        };

        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build(1, 10);

        assert!(sql.contains("FROM \"item\""));
        assert!(sql.contains("stage_id"));
        assert!(sql.contains("LIMIT 10"));
        assert!(sql.contains("ORDER BY"));
    }

    #[test]
    fn count_query_build() {
        let def = QueryDefinition {
            base_table: "item".to_string(),
            item_type: Some("blog".to_string()),
            ..Default::default()
        };

        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build_count();

        assert!(sql.contains("COUNT(*)"));
        assert!(sql.contains("FROM \"item\""));
        assert!(!sql.contains("LIMIT"));
    }

    #[test]
    fn jsonb_field_query() {
        let def = QueryDefinition {
            base_table: "item".to_string(),
            fields: vec![QueryField {
                field_name: "fields.body".to_string(),
                table_alias: None,
                label: Some("body".to_string()),
            }],
            ..Default::default()
        };

        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build(1, 10);

        assert!(sql.contains("fields->>'body'"));
    }

    #[test]
    fn category_descendants_cte() {
        let tag_id = uuid::Uuid::nil();
        let cte = CategoryHierarchyQuery::descendants_cte(tag_id);

        assert!(cte.contains("WITH RECURSIVE"));
        assert!(cte.contains("tag_descendants"));
        assert!(cte.contains("category_tag_hierarchy"));
    }

    #[test]
    fn pagination_offset() {
        let def = QueryDefinition::default();
        let builder = GatherQueryBuilder::new(def, "live");

        let sql_page1 = builder.build(1, 10);
        assert!(sql_page1.contains("OFFSET 0"));

        let def2 = QueryDefinition::default();
        let builder2 = GatherQueryBuilder::new(def2, "live");
        let sql_page2 = builder2.build(2, 10);
        assert!(sql_page2.contains("OFFSET 10"));
    }

    #[test]
    fn filter_operators() {
        let def = QueryDefinition {
            base_table: "item".to_string(),
            filters: vec![QueryFilter {
                field: "title".to_string(),
                operator: FilterOperator::Contains,
                value: FilterValue::String("rust".to_string()),
                exposed: false,
                exposed_label: None,
            }],
            ..Default::default()
        };

        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build(1, 10);

        assert!(sql.contains("LIKE"));
        assert!(sql.contains("%rust%"));
    }

    #[test]
    fn stage_aware_false_omits_stage_filter() {
        let def = QueryDefinition {
            base_table: "users".to_string(),
            stage_aware: false,
            ..Default::default()
        };

        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build(1, 20);

        assert!(sql.contains("FROM \"users\""));
        assert!(
            !sql.contains("stage_id"),
            "stage_id should not appear when stage_aware=false"
        );
        assert!(sql.contains("LIMIT 20"));
    }

    #[test]
    fn stage_aware_false_count_omits_stage_filter() {
        let def = QueryDefinition {
            base_table: "users".to_string(),
            stage_aware: false,
            ..Default::default()
        };

        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build_count();

        assert!(sql.contains("COUNT(*)"));
        assert!(sql.contains("FROM \"users\""));
        assert!(
            !sql.contains("stage_id"),
            "stage_id should not appear in count when stage_aware=false"
        );
    }

    #[test]
    fn stage_aware_default_true() {
        let def = QueryDefinition::default();
        assert!(def.stage_aware, "stage_aware should default to true");

        let builder = GatherQueryBuilder::new(def, "preview");
        let sql = builder.build(1, 10);
        assert!(
            sql.contains("stage_id"),
            "stage_id should appear when stage_aware=true (default)"
        );
    }

    #[test]
    fn stage_aware_false_deserializes_from_json() {
        let json = r#"{"base_table": "users", "stage_aware": false}"#;
        let def: QueryDefinition = serde_json::from_str(json).unwrap();
        assert!(!def.stage_aware);
        assert_eq!(def.base_table, "users");
    }

    #[test]
    fn stage_aware_missing_defaults_true() {
        let json = r#"{"base_table": "item"}"#;
        let def: QueryDefinition = serde_json::from_str(json).unwrap();
        assert!(
            def.stage_aware,
            "stage_aware should default to true when not in JSON"
        );
    }

    #[test]
    fn stage_overlay_single_stage_uses_equals() {
        let def = QueryDefinition::default();
        let builder = GatherQueryBuilder::new_with_stages(def, vec!["live".to_string()]);
        let sql = builder.build(1, 10);

        // Single stage should use = 'live'
        assert!(
            sql.contains("\"stage_id\" = 'live'"),
            "single stage should use =: {sql}"
        );
        assert!(!sql.contains("IN"), "single stage should not use IN: {sql}");
    }

    #[test]
    fn stage_overlay_multiple_stages_uses_in() {
        let def = QueryDefinition::default();
        let stages = vec![
            "review".to_string(),
            "draft".to_string(),
            "live".to_string(),
        ];
        let builder = GatherQueryBuilder::new_with_stages(def, stages);
        let sql = builder.build(1, 10);

        // Multiple stages should use IN
        assert!(
            sql.contains("IN"),
            "multiple stages should use IN clause: {sql}"
        );
        assert!(sql.contains("'review'"), "should contain review: {sql}");
        assert!(sql.contains("'draft'"), "should contain draft: {sql}");
        assert!(sql.contains("'live'"), "should contain live: {sql}");
    }

    #[test]
    fn stage_overlay_count_uses_in() {
        let def = QueryDefinition::default();
        let stages = vec!["review".to_string(), "live".to_string()];
        let builder = GatherQueryBuilder::new_with_stages(def, stages);
        let sql = builder.build_count();

        assert!(
            sql.contains("IN"),
            "count with multiple stages should use IN: {sql}"
        );
        assert!(
            sql.contains("'review'"),
            "count should contain review: {sql}"
        );
        assert!(sql.contains("'live'"), "count should contain live: {sql}");
    }

    #[test]
    fn full_text_search_filter() {
        let def = QueryDefinition {
            base_table: "item".to_string(),
            filters: vec![QueryFilter {
                field: "search_vector".to_string(),
                operator: FilterOperator::FullTextSearch,
                value: FilterValue::String("rust programming".to_string()),
                exposed: false,
                exposed_label: None,
            }],
            ..Default::default()
        };

        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build(1, 10);

        assert!(
            sql.contains("search_vector @@ to_tsquery"),
            "should contain tsvector search: {sql}"
        );
        // Parameterized: value appears as 'rust & programming' after Expr::cust_with_values
        assert!(
            sql.contains("rust & programming"),
            "should AND terms: {sql}"
        );
    }

    #[test]
    fn full_text_search_empty_value_skipped() {
        let def = QueryDefinition {
            base_table: "item".to_string(),
            filters: vec![QueryFilter {
                field: "search_vector".to_string(),
                operator: FilterOperator::FullTextSearch,
                value: FilterValue::String("".to_string()),
                exposed: false,
                exposed_label: None,
            }],
            ..Default::default()
        };

        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build(1, 10);

        assert!(
            !sql.contains("search_vector"),
            "empty search should be skipped: {sql}"
        );
    }

    #[test]
    fn full_text_search_sanitizes_special_chars() {
        let def = QueryDefinition {
            base_table: "item".to_string(),
            filters: vec![QueryFilter {
                field: "search_vector".to_string(),
                operator: FilterOperator::FullTextSearch,
                value: FilterValue::String("rust's | ! & (test)".to_string()),
                exposed: false,
                exposed_label: None,
            }],
            ..Default::default()
        };

        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build(1, 10);

        // Special chars should be stripped, only words remain
        assert!(
            sql.contains("search_vector @@ to_tsquery"),
            "should contain search: {sql}"
        );
        assert!(!sql.contains("|"), "pipe should be stripped: {sql}");
        assert!(!sql.contains("!"), "bang should be stripped: {sql}");
    }

    #[test]
    fn full_text_search_operator_serialization() {
        let op = FilterOperator::FullTextSearch;
        let json = serde_json::to_string(&op).unwrap();
        assert_eq!(json, "\"full_text_search\"");
        let parsed: FilterOperator = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, FilterOperator::FullTextSearch);
    }

    #[test]
    fn like_wildcards_escaped() {
        let def = QueryDefinition {
            base_table: "item".to_string(),
            filters: vec![QueryFilter {
                field: "title".to_string(),
                operator: FilterOperator::Contains,
                value: FilterValue::String("100%_done".to_string()),
                exposed: false,
                exposed_label: None,
            }],
            ..Default::default()
        };

        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build(1, 10);

        // SeaQuery renders with E prefix and double-backslash escaping
        assert!(
            sql.contains("100\\\\%\\\\_done") || sql.contains("100\\%\\_done"),
            "LIKE wildcards should be escaped: {sql}"
        );
        // The important thing: literal % and _ are escaped, not used as wildcards
        assert!(
            !sql.contains("%100%_done%"),
            "raw wildcard chars should NOT appear unescaped: {sql}"
        );
    }

    #[test]
    fn escape_like_wildcards_function() {
        assert_eq!(super::escape_like_wildcards("hello"), "hello");
        assert_eq!(super::escape_like_wildcards("100%"), "100\\%");
        assert_eq!(super::escape_like_wildcards("a_b"), "a\\_b");
        assert_eq!(super::escape_like_wildcards("a\\b"), "a\\\\b");
    }

    #[test]
    fn stage_overlay_not_applied_when_not_stage_aware() {
        let def = QueryDefinition {
            base_table: "users".to_string(),
            stage_aware: false,
            ..Default::default()
        };
        let stages = vec!["review".to_string(), "live".to_string()];
        let builder = GatherQueryBuilder::new_with_stages(def, stages);
        let sql = builder.build(1, 10);

        assert!(
            !sql.contains("stage_id"),
            "stage_aware=false should skip stage filter even with overlay: {sql}"
        );
    }

    // ── Security regression tests ──
    // Note: is_safe_identifier() is tested in handlers::tests

    #[test]
    fn jsonb_path_injection_returns_null() {
        let def = QueryDefinition {
            base_table: "item".to_string(),
            fields: vec![QueryField {
                field_name: "fields.body'; DROP TABLE item; --".to_string(),
                table_alias: None,
                label: Some("injected".to_string()),
            }],
            ..Default::default()
        };
        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build(1, 10);

        // The unsafe path is neutralized to a bare NULL expression.
        // Check that no JSONB extraction operator appears with the injection payload.
        assert!(
            sql.contains("NULL AS"),
            "unsafe path should produce NULL alias expression: {sql}"
        );
        assert!(
            !sql.contains("->>"),
            "should not generate JSONB extraction for unsafe path: {sql}"
        );
    }

    #[test]
    fn jsonb_nested_path_injection_returns_null() {
        let def = QueryDefinition {
            base_table: "item".to_string(),
            fields: vec![QueryField {
                field_name: "fields.meta.source'; DROP TABLE item;--".to_string(),
                table_alias: None,
                label: Some("injected".to_string()),
            }],
            ..Default::default()
        };
        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build(1, 10);

        // Nested path with injection should also be neutralized to NULL.
        assert!(
            sql.contains("NULL AS"),
            "unsafe nested path should produce NULL alias expression: {sql}"
        );
        assert!(
            !sql.contains("->>"),
            "should not generate JSONB extraction for unsafe nested path: {sql}"
        );
    }

    #[test]
    fn jsonb_table_injection_returns_null() {
        // When base_table contains injection, jsonb_extract_expr returns NULL.
        // SeaQuery safely quotes the base_table in FROM via Alias::new(),
        // so it appears as a quoted identifier (harmless at SQL level).
        let def = QueryDefinition {
            base_table: "item; DROP TABLE users".to_string(),
            fields: vec![QueryField {
                field_name: "fields.body".to_string(),
                table_alias: None,
                label: Some("body".to_string()),
            }],
            stage_aware: false,
            ..Default::default()
        };
        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build(1, 10);

        // JSONB expression neutralized to NULL (not the raw interpolation).
        // The unsafe table name should not appear before a JSONB operator.
        assert!(
            sql.contains("NULL AS"),
            "unsafe base table should yield NULL expression: {sql}"
        );
        assert!(
            !sql.contains(".fields->>"),
            "should not interpolate unsafe table into JSONB expression: {sql}"
        );
    }

    #[test]
    fn category_filter_uses_parameterized_values() {
        let tag_id = uuid::Uuid::nil();
        let def = QueryDefinition {
            base_table: "item".to_string(),
            filters: vec![QueryFilter {
                field: "fields.category".to_string(),
                operator: FilterOperator::HasTag,
                value: FilterValue::Uuid(tag_id),
                exposed: false,
                exposed_label: None,
            }],
            ..Default::default()
        };
        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build(1, 10);

        // UUID should appear as a SeaQuery-parameterized value, not a format! interpolation.
        // The expression should use JSONB extraction via ->> and an IN clause.
        assert!(
            sql.contains("->>"),
            "should use JSONB extraction for category field: {sql}"
        );
        assert!(sql.contains("IN"), "should use IN clause: {sql}");
        assert!(
            sql.contains(&tag_id.to_string()),
            "should contain UUID value: {sql}"
        );
    }

    #[test]
    fn category_filter_field_injection_blocked() {
        let def = QueryDefinition {
            base_table: "item".to_string(),
            filters: vec![QueryFilter {
                field: "fields.cat' OR '1'='1".to_string(),
                operator: FilterOperator::HasTag,
                value: FilterValue::Uuid(uuid::Uuid::nil()),
                exposed: false,
                exposed_label: None,
            }],
            ..Default::default()
        };
        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build(1, 10);

        // Injection payload should be neutralized to (NULL) IN (...), not executed.
        assert!(
            sql.contains("(NULL) IN"),
            "unsafe field should produce NULL expression: {sql}"
        );
        assert!(
            !sql.contains("'1'='1'"),
            "SQL injection payload should not appear in output: {sql}"
        );
    }

    #[test]
    fn fts_unsafe_base_table_returns_false() {
        // When base_table contains injection, FTS filter returns FALSE.
        // SeaQuery safely quotes the base_table in FROM via Alias::new().
        let def = QueryDefinition {
            base_table: "item; DROP TABLE users".to_string(),
            stage_aware: false,
            filters: vec![QueryFilter {
                field: "search_vector".to_string(),
                operator: FilterOperator::FullTextSearch,
                value: FilterValue::String("test".to_string()),
                exposed: false,
                exposed_label: None,
            }],
            ..Default::default()
        };
        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build(1, 10);

        // FTS expression neutralized to FALSE (not interpolated into search_vector).
        // The WHERE clause should contain FALSE, and no @@ operator should appear.
        assert!(
            sql.contains("FALSE"),
            "should return FALSE for unsafe base table: {sql}"
        );
        assert!(
            !sql.contains("@@"),
            "should not generate FTS operator for unsafe base table: {sql}"
        );
    }

    #[test]
    fn sort_field_injection_returns_null() {
        let def = QueryDefinition {
            base_table: "item".to_string(),
            sorts: vec![QuerySort {
                field: "fields.x'; DROP TABLE item;--".to_string(),
                direction: SortDirection::Asc,
                nulls: None,
            }],
            ..Default::default()
        };
        let builder = GatherQueryBuilder::new(def, "live");
        let sql = builder.build(1, 10);

        // Sort expression should be neutralized to NULL ORDER BY, not raw injection.
        assert!(
            !sql.contains("->>"),
            "should not generate JSONB extraction for unsafe sort field: {sql}"
        );
        assert!(
            sql.contains("NULL"),
            "unsafe sort field should produce NULL expression: {sql}"
        );
    }

    /// Helper: render a SimpleExpr to SQL string via a dummy SELECT.
    fn expr_to_sql(expr: SimpleExpr) -> String {
        let mut q = Query::select();
        q.expr(expr);
        q.to_string(PostgresQueryBuilder)
    }

    #[test]
    fn descendants_expr_injection_returns_false() {
        let result = CategoryHierarchyQuery::in_descendants_expr("cat'; DROP TABLE item;--");
        let sql = expr_to_sql(result);
        assert!(
            sql.contains("FALSE"),
            "unsafe field path should produce FALSE: {sql}"
        );
        assert!(
            !sql.contains("fields"),
            "should not generate JSONB extraction for unsafe path: {sql}"
        );
    }

    #[test]
    fn descendants_expr_valid_path() {
        let result = CategoryHierarchyQuery::in_descendants_expr("category");
        let sql = expr_to_sql(result);
        assert!(sql.contains("category"), "should contain field path: {sql}");
        assert!(
            sql.contains("tag_descendants"),
            "should reference descendants CTE: {sql}"
        );
    }
}
