//! Gather query builder using SeaQuery.
//!
//! Generates type-safe SQL queries from QueryDefinition with support for:
//! - JSONB field extraction
//! - Category hierarchy filters
//! - Stage-aware queries
//! - Pagination

use super::extension::{FilterContext, GatherExtensionRegistry};
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
    stage_id: String,
    extensions: Option<Arc<GatherExtensionRegistry>>,
}

impl GatherQueryBuilder {
    /// Create a new query builder.
    pub fn new(definition: QueryDefinition, stage_id: &str) -> Self {
        Self {
            definition,
            stage_id: stage_id.to_string(),
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

        // Always filter by stage
        query.and_where(
            Expr::col((
                Alias::new(&self.definition.base_table),
                Alias::new("stage_id"),
            ))
            .eq(&self.stage_id),
        );

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

        // Stage filter
        query.and_where(
            Expr::col((
                Alias::new(&self.definition.base_table),
                Alias::new("stage_id"),
            ))
            .eq(&self.stage_id),
        );

        // Item type filter
        if let Some(ref item_type) = self.definition.item_type {
            query.and_where(
                Expr::col((Alias::new(&self.definition.base_table), Alias::new("type")))
                    .eq(item_type),
            );
        }

        query.to_string(PostgresQueryBuilder)
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
                Some(field_expr.like(format!("%{}%", value)))
            }
            FilterOperator::StartsWith => {
                let value = filter.value.as_string()?;
                Some(field_expr.like(format!("{}%", value)))
            }
            FilterOperator::EndsWith => {
                let value = filter.value.as_string()?;
                Some(field_expr.like(format!("%{}", value)))
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
                if let Some(ref extensions) = self.extensions {
                    if let Some((handler, config)) = extensions.get_filter(name) {
                        let ctx = FilterContext {
                            base_table: self.definition.base_table.clone(),
                            stage_id: self.stage_id.clone(),
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
        if field.starts_with("fields.") {
            let jsonb_path = &field[7..];
            self.jsonb_extract_expr(&self.definition.base_table, jsonb_path)
        } else {
            Expr::col((Alias::new(&self.definition.base_table), Alias::new(field))).into()
        }
    }

    /// Extract a value from a JSONB column.
    fn jsonb_extract_expr(&self, table: &str, path: &str) -> SimpleExpr {
        // Use ->> for text extraction from JSONB
        // e.g., fields->>'body' for fields.body
        if path.contains('.') {
            // Nested path: fields->'nested'->>'field'
            let parts: Vec<&str> = path.split('.').collect();
            let mut expr = format!("{}.fields", table);
            for (i, part) in parts.iter().enumerate() {
                if i == parts.len() - 1 {
                    expr = format!("({}->>'{}')", expr, part);
                } else {
                    expr = format!("({}->'{}')", expr, part);
                }
            }
            Expr::cust(expr)
        } else {
            Expr::cust(format!("{}.fields->>'{}'", table, path))
        }
    }

    /// Build a category filter condition.
    /// If `include_descendants` is true, matches the tag or any of its descendants.
    fn build_category_filter(
        &self,
        field: &str,
        tag_ids: Vec<uuid::Uuid>,
        include_descendants: bool,
    ) -> Option<SimpleExpr> {
        let jsonb_path = if field.starts_with("fields.") {
            &field[7..]
        } else {
            field
        };

        // Build the list of UUIDs to check
        let uuid_list: Vec<String> = tag_ids.iter().map(|u| format!("'{}'", u)).collect();

        if include_descendants {
            // Use a subquery with recursive CTE to get all descendants
            // For simplicity, we'll use a parameterized IN clause that the caller
            // must expand with actual descendant IDs
            // In practice, this would be resolved before query building
            let expr = format!(
                "{}.fields->>'{}' IN ({})",
                self.definition.base_table,
                jsonb_path,
                uuid_list.join(", ")
            );
            Some(Expr::cust(expr))
        } else {
            // Simple IN check
            let expr = format!(
                "{}.fields->>'{}' IN ({})",
                self.definition.base_table,
                jsonb_path,
                uuid_list.join(", ")
            );
            Some(Expr::cust(expr))
        }
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

/// Builder for creating category hierarchy subqueries.
#[allow(dead_code)]
pub struct CategoryHierarchyQuery;

#[allow(dead_code)]
impl CategoryHierarchyQuery {
    /// Build a recursive CTE to get a tag and all its descendants.
    /// Returns the SQL for the WITH clause that can be prepended to the main query.
    pub fn descendants_cte(tag_id: uuid::Uuid) -> String {
        format!(
            r#"WITH RECURSIVE tag_descendants AS (
    SELECT '{}'::uuid as id
    UNION ALL
    SELECT h.tag_id
    FROM category_tag_hierarchy h
    INNER JOIN tag_descendants d ON h.parent_id = d.id
)"#,
            tag_id
        )
    }

    /// Build a filter expression that checks if a JSONB field is in the descendants CTE.
    pub fn in_descendants_expr(field_path: &str) -> String {
        format!(
            "(fields->>'{}')::uuid IN (SELECT id FROM tag_descendants)",
            field_path
        )
    }
}

#[cfg(test)]
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
}
