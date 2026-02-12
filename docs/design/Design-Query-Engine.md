# Trovato Design: Gather Query Engine

*Section 9 of the v2.1 Design Document*

---

## 9. The Query Builder (Gather Engine)

### What Gather Actually Does

In Drupal 6, Gather is a plugin that lets you build database queries through a UI. Under the hood, it's a SQL query builder that starts with a base table, adds SELECT columns, WHERE clauses, ORDER BY, JOINs, contextual filters from the URL, LIMIT/OFFSET, then passes results through a style plugin for rendering.

We use SeaQuery because it builds SQL as an AST rather than string concatenation — safe and dialect-agnostic.

### The View Definition

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewDefinition {
    pub name: String,
    pub base_table: String,
    pub displays: Vec<ViewDisplay>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewDisplay {
    pub id: String,
    pub display_type: DisplayType,
    pub fields: Vec<ViewField>,
    pub filters: Vec<ViewFilter>,
    pub sorts: Vec<ViewSort>,
    pub relationships: Vec<ViewRelationship>,
    pub arguments: Vec<ViewArgument>,
    pub pager: PagerSettings,
    pub path: Option<String>,
    pub style: StylePlugin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewField {
    pub id: String,
    pub table: String,
    pub column: String,
    pub label: Option<String>,
    pub exclude: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewFilter {
    pub table: String,
    pub column: String,
    pub operator: FilterOperator,
    pub value: serde_json::Value,
    pub exposed: bool,
    pub group: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FilterOperator {
    Equals, NotEquals, Contains, StartsWith,
    GreaterThan, LessThan,
    GreaterThanOrEqual, LessThanOrEqual,
    In, NotIn, IsNull, IsNotNull, Between,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewRelationship {
    pub id: String,
    pub base_table: String,
    pub base_column: String,
    pub target_table: String,
    pub target_column: String,
    pub join_type: JoinType,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewArgument {
    pub table: String,
    pub column: String,
    pub position: usize,
    pub default_action: ArgumentDefault,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArgumentDefault {
    Ignore, NotFound, Fixed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewSort {
    pub table: String,
    pub column: String,
    pub direction: SortDirection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SortDirection {
    Asc, Desc,
}
```

### Building the Query

```rust
use sea_query::{Alias, Cond, Expr, Order, PostgresQueryBuilder, Query};

pub struct ViewQueryBuilder;

impl ViewQueryBuilder {
    pub fn build(
        display: &ViewDisplay,
        base_table: &str,
        url_arguments: &[String],
    ) -> Result<(String, Vec<sea_query::Value>), ViewError> {
        let mut query = Query::select();
        let base = Alias::new(base_table);
        query.from(base.clone());

        for field in &display.fields {
            if !is_base_column(&field.column) {
                query.expr_as(
                    Expr::cust(format!(
                        "\"{}\".\"fields\"->'{}'->>'value'",
                        base_table, field.column
                    )),
                    Alias::new(&field.id),
                );
            } else {
                query.column((
                    Alias::new(&field.table),
                    Alias::new(&field.column),
                ));
            }
        }

        for rel in &display.relationships {
            let jt = if rel.required {
                sea_query::JoinType::InnerJoin
            } else {
                sea_query::JoinType::LeftJoin
            };
            query.join(
                jt, Alias::new(&rel.target_table),
                Expr::col((
                    Alias::new(&rel.base_table),
                    Alias::new(&rel.base_column),
                )).equals((
                    Alias::new(&rel.target_table),
                    Alias::new(&rel.target_column),
                )),
            );
        }

        let mut conditions = Cond::all();
        for filter in &display.filters {
            let is_jsonb = !is_base_column(&filter.column);

            // For JSONB fields, the ->> operator always returns text.
            // For numeric comparisons (GT, LT, GTE, LTE, Between), we
            // must cast to the appropriate type. The filter value's JSON
            // type determines the cast: number → ::numeric, else text.
            let needs_numeric_cast = is_jsonb && matches!(
                &filter.operator,
                FilterOperator::GreaterThan
                | FilterOperator::LessThan
                | FilterOperator::GreaterThanOrEqual
                | FilterOperator::LessThanOrEqual
                | FilterOperator::Between
            ) && filter.value.is_number();

            let col_expr = if is_jsonb {
                if needs_numeric_cast {
                    Expr::cust(format!(
                        "(\"{}\".\"fields\"->'{}'->>'value')::numeric",
                        filter.table, filter.column
                    ))
                } else {
                    Expr::cust(format!(
                        "\"{}\".\"fields\"->'{}'->>'value'",
                        filter.table, filter.column
                    ))
                }
            } else {
                Expr::col((
                    Alias::new(&filter.table),
                    Alias::new(&filter.column),
                ))
            };

            let expr = match &filter.operator {
                FilterOperator::Equals =>
                    col_expr.eq(json_to_sea_value(&filter.value)),
                FilterOperator::NotEquals =>
                    col_expr.ne(json_to_sea_value(&filter.value)),
                FilterOperator::Contains => {
                    let s = filter.value.as_str().unwrap_or("");
                    col_expr.like(format!("%{s}%"))
                }
                FilterOperator::StartsWith => {
                    let s = filter.value.as_str().unwrap_or("");
                    col_expr.like(format!("{s}%"))
                }
                FilterOperator::GreaterThan =>
                    col_expr.gt(json_to_sea_value(&filter.value)),
                FilterOperator::LessThan =>
                    col_expr.lt(json_to_sea_value(&filter.value)),
                FilterOperator::GreaterThanOrEqual =>
                    col_expr.gte(json_to_sea_value(&filter.value)),
                FilterOperator::LessThanOrEqual =>
                    col_expr.lte(json_to_sea_value(&filter.value)),
                FilterOperator::Between => {
                    let arr = filter.value.as_array();
                    match arr {
                        Some(a) if a.len() == 2 =>
                            col_expr.between(
                                json_to_sea_value(&a[0]),
                                json_to_sea_value(&a[1]),
                            ),
                        _ => return Err(ViewError::InvalidFilterValue(
                            format!("{}: Between requires [min, max] array",
                                    filter.column)
                        )),
                    }
                }
                FilterOperator::IsNull => col_expr.is_null(),
                FilterOperator::IsNotNull => col_expr.is_not_null(),
                FilterOperator::In => {
                    let values: Vec<sea_query::Value> =
                        filter.value.as_array()
                            .unwrap_or(&vec![])
                            .iter()
                            .map(json_to_sea_value)
                            .collect();
                    col_expr.is_in(values)
                }
                FilterOperator::NotIn => {
                    let values: Vec<sea_query::Value> =
                        filter.value.as_array()
                            .unwrap_or(&vec![])
                            .iter()
                            .map(json_to_sea_value)
                            .collect();
                    col_expr.is_not_in(values)
                }
            };
            conditions = conditions.add(expr);
        }
        query.cond_where(conditions);

        for arg in &display.arguments {
            match (url_arguments.get(arg.position), &arg.default_action) {
                (Some(val), _) => {
                    query.and_where(
                        Expr::col((
                            Alias::new(&arg.table),
                            Alias::new(&arg.column),
                        )).eq(val.as_str())
                    );
                }
                (None, ArgumentDefault::Ignore) => {}
                (None, ArgumentDefault::NotFound) => {
                    return Err(ViewError::MissingArgument(arg.position));
                }
                (None, ArgumentDefault::Fixed(default)) => {
                    query.and_where(
                        Expr::col((
                            Alias::new(&arg.table),
                            Alias::new(&arg.column),
                        )).eq(default.as_str())
                    );
                }
            }
        }

        for sort in &display.sorts {
            let order = match sort.direction {
                SortDirection::Asc => Order::Asc,
                SortDirection::Desc => Order::Desc,
            };
            if !is_base_column(&sort.column) {
                // JSONB fields: sort by extracted text value.
                // For numeric sorting, an expression index with a cast
                // (see "Expression Indexes" below) is required. Without
                // it, sorting is lexicographic on the text representation.
                query.order_by(
                    Expr::cust(format!(
                        "\"{}\".\"fields\"->'{}'->>'value'",
                        sort.table, sort.column
                    )),
                    order,
                );
            } else {
                query.order_by(
                    (Alias::new(&sort.table), Alias::new(&sort.column)),
                    order,
                );
            }
        }

        query.limit(display.pager.items_per_page);
        if display.pager.offset > 0 {
            query.offset(display.pager.offset);
        }

        Ok(query.build(PostgresQueryBuilder))
    }
}

### Stage-Aware Gather Queries

When executing a Gather query in a stage context, the query must:
1. Include items from both the Live stage and the active stage
2. Exclude items marked as deleted in the active stage
3. Use stage revision overrides for modified items

The Gather engine achieves this by wrapping the base item query with stage filters. Rather than modifying every query, the `ViewQueryBuilder` accepts an optional `stage_id` and applies the stage CTE as a prefix:

```rust
impl ViewQueryBuilder {
    /// Wraps a Gather query with stage-awareness.
    /// When stage_id is Some, the query uses stage revision
    /// overrides and excludes stage deletions.
    pub fn with_stage(
        query_sql: &str,
        stage_id: Option<&str>,
    ) -> String {
        match stage_id {
            None => query_sql.to_string(),
            Some(st) => format!(
                "WITH stage_items AS (
                    SELECT i.id, i.type, i.author_id, i.status, i.promote, i.sticky,
                           COALESCE(r.title, i.title) as title,
                           COALESCE(r.fields, i.fields) as fields,
                           COALESCE(r.created, i.created) as created,
                           i.changed, i.search_vector
                    FROM item i
                    LEFT JOIN stage_association sa
                        ON sa.stage_id = '{st}' AND sa.item_id = i.id
                    LEFT JOIN item_revision r ON r.id = sa.target_revision_id
                    WHERE (i.stage_id = 'live' OR i.stage_id = '{st}')
                      AND NOT EXISTS (
                          SELECT 1 FROM stage_deletion sd
                          WHERE sd.stage_id = '{st}'
                            AND sd.entity_type = 'item'
                            AND sd.entity_id = i.id
                      )
                )
                {query_sql}",
                query_sql = query_sql.replace("FROM item", "FROM stage_items AS item")
            ),
        }
    }
}
```

This approach keeps the core query builder stage-agnostic. The stage CTE is applied as a transparent layer that replaces the `item` table with a stage-aware view. All filters, sorts, and field references work identically.

**Performance note:** The CTE adds overhead per query. For the Live stage (the common case), no CTE is needed — the query runs directly against the `item` table with `WHERE stage_id = 'live'`. The CTE only activates when a user is previewing a non-live stage.

fn is_base_column(name: &str) -> bool {
    matches!(
        name,
        "id" | "current_revision_id" | "type" | "title" | "author_id"
        | "status" | "created" | "changed"
        | "promote" | "sticky"
    )
}
```

### JSONB Query Performance and Expression Indexes

GIN indexes on JSONB work well for containment queries (`@>` operator) but not for arbitrary path queries with comparison operators. For `field_price > 100`, the GIN index won't help.

For high-traffic Gather with complex field filters, admins can define "Expression Indexes" in the item type configuration. The Kernel manages the creation of these physical Postgres indexes:

```sql
CREATE INDEX idx_node_price ON item (
    (fields->'field_price'->>'value')::integer
);
```

This is a real limitation of the JSONB approach. EAV tables were slow for JOINs but fast for indexed lookups. JSONB is the opposite tradeoff. For most sites (low tens of thousands of items), JSONB is fine. For millions of items with complex field filters, you'll need expression indexes.

**Materialized cache:** For expensive Gather, cache results in Redis and invalidate on item save using cache tags (see Section 14).

---

