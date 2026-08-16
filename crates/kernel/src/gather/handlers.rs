//! Built-in Gather extension handlers.
//!
//! These are kernel-side Rust implementations that plugins activate
//! via `tap_gather_extend` JSON declarations.

use anyhow::{Result, bail};
use sea_query::{Expr, SimpleExpr};
use sqlx::PgPool;
use std::future::Future;
use std::pin::Pin;
use uuid::Uuid;

use super::extension::{FilterContext, FilterHandler};
use super::types::{FilterValue, QueryFilter};

/// Maximum recursion depth for hierarchy CTEs.
const MAX_CTE_DEPTH: i64 = 100;

/// Validate a SQL identifier name (table/column names).
/// Allows only `[a-zA-Z_][a-zA-Z0-9_]*` with max 63 chars (PostgreSQL limit).
pub(super) fn is_safe_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
}

/// Validate a JSONB field path (e.g. "category" or "meta.source").
/// Each dot-separated segment must be a safe identifier.
fn is_safe_jsonb_path(path: &str) -> bool {
    !path.is_empty() && path.split('.').all(is_safe_identifier)
}

// ---------------------------------------------------------------------------
// HierarchicalInFilterHandler
// ---------------------------------------------------------------------------

/// Filter handler that expands a root UUID to all its descendants via a
/// recursive CTE, then generates an `IN (...)` clause.
///
/// Config keys:
/// - `hierarchy_table`: Table containing the hierarchy (e.g. "category_tag_hierarchy")
/// - `id_column`: Child ID column (e.g. "tag_id")
/// - `parent_column`: Parent ID column (e.g. "parent_id")
/// - `expand_descendants`: If true, run recursive CTE to find descendants
pub struct HierarchicalInFilterHandler;

impl FilterHandler for HierarchicalInFilterHandler {
    fn build_condition(
        &self,
        filter: &QueryFilter,
        _config: &serde_json::Value,
        ctx: &FilterContext,
    ) -> Result<Option<SimpleExpr>> {
        // By the time build_condition is called, resolve() has already expanded
        // the UUIDs. We just need to generate the IN clause.
        let uuids = filter.value.as_uuid_list();
        if uuids.is_empty() {
            return Ok(None);
        }

        let jsonb_path = filter
            .field
            .strip_prefix("fields.")
            .unwrap_or(&filter.field);

        if !is_safe_jsonb_path(jsonb_path) {
            bail!("unsafe JSONB field path: '{jsonb_path}'");
        }

        // Defense-in-depth: validate base_table before interpolation
        if !is_safe_identifier(&ctx.base_table) {
            bail!(
                "unsafe base_table name: '{}'",
                &ctx.base_table[..ctx.base_table.len().min(64)]
            );
        }

        let uuid_list: Vec<String> = uuids.iter().map(|u| format!("'{u}'")).collect();
        let expr = format!(
            "{}.fields->>'{}' IN ({})",
            ctx.base_table,
            jsonb_path,
            uuid_list.join(", ")
        );

        Ok(Some(Expr::cust(expr)))
    }

    fn resolve<'a>(
        &'a self,
        filter: QueryFilter,
        config: &'a serde_json::Value,
        pool: &'a PgPool,
    ) -> Pin<Box<dyn Future<Output = Result<QueryFilter>> + Send + 'a>> {
        Box::pin(async move {
            let expand = config
                .get("expand_descendants")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if !expand {
                return Ok(filter);
            }

            let hierarchy_table = config
                .get("hierarchy_table")
                .and_then(|v| v.as_str())
                .unwrap_or("category_tag_hierarchy");
            let id_column = config
                .get("id_column")
                .and_then(|v| v.as_str())
                .unwrap_or("tag_id");
            let parent_column = config
                .get("parent_column")
                .and_then(|v| v.as_str())
                .unwrap_or("parent_id");

            // Validate identifier safety
            if !is_safe_identifier(hierarchy_table) {
                bail!("unsafe hierarchy_table name: '{hierarchy_table}'");
            }
            if !is_safe_identifier(id_column) {
                bail!("unsafe id_column name: '{id_column}'");
            }
            if !is_safe_identifier(parent_column) {
                bail!("unsafe parent_column name: '{parent_column}'");
            }

            // Extract root UUIDs — supports single UUID or List of UUIDs
            let root_ids: Vec<Uuid> = match &filter.value {
                FilterValue::Uuid(id) => vec![*id],
                FilterValue::List(items) => items.iter().filter_map(|v| v.as_uuid()).collect(),
                FilterValue::String(s) => match Uuid::parse_str(s) {
                    Ok(id) => vec![id],
                    Err(_) => return Ok(filter),
                },
                _ => return Ok(filter),
            };

            if root_ids.is_empty() {
                return Ok(filter);
            }

            let max_depth = config
                .get("max_depth")
                .and_then(|v| v.as_i64())
                .unwrap_or(MAX_CTE_DEPTH);

            // Run recursive CTE to expand descendants, using bind parameter
            // for root IDs and a depth limit to prevent runaway recursion.
            let sql = format!(
                r#"WITH RECURSIVE descendants AS (
    SELECT unnest($1::uuid[]) AS id, 0 AS depth
    UNION ALL
    SELECT h.{id_column}, d.depth + 1
    FROM {hierarchy_table} h
    INNER JOIN descendants d ON h.{parent_column} = d.id
    WHERE d.depth < {max_depth}
)
SELECT DISTINCT id FROM descendants"#,
            );

            let rows: Vec<Uuid> = sqlx::query_scalar(&sql)
                .bind(&root_ids[..])
                .fetch_all(pool)
                .await?;

            let expanded_value =
                FilterValue::List(rows.into_iter().map(FilterValue::Uuid).collect());

            Ok(QueryFilter {
                value: expanded_value,
                ..filter
            })
        })
    }
}

// ---------------------------------------------------------------------------
// JsonbArrayContainsFilterHandler
// ---------------------------------------------------------------------------

/// Filter handler that generates a JSONB `@>` containment check.
///
/// Generates: `base_table.fields->'field' @> '["value"]'::jsonb`
///
/// No resolve phase needed.
pub struct JsonbArrayContainsFilterHandler;

impl FilterHandler for JsonbArrayContainsFilterHandler {
    /// Build a JSONB array containment condition.
    ///
    /// # Panics
    ///
    /// Panics if `serde_json` fails to serialize a single-element string array.
    /// This is infallible for valid string values.
    fn build_condition(
        &self,
        filter: &QueryFilter,
        _config: &serde_json::Value,
        ctx: &FilterContext,
    ) -> Result<Option<SimpleExpr>> {
        let Some(value_str) = filter.value.as_string() else {
            return Ok(None);
        };

        let jsonb_path = filter
            .field
            .strip_prefix("fields.")
            .unwrap_or(&filter.field);

        if !is_safe_jsonb_path(jsonb_path) {
            bail!("unsafe JSONB field path: '{jsonb_path}'");
        }

        // Defense-in-depth: validate base_table before interpolation
        if !is_safe_identifier(&ctx.base_table) {
            bail!(
                "unsafe base_table name: '{}'",
                &ctx.base_table[..ctx.base_table.len().min(64)]
            );
        }

        // Use serde_json for correct escaping of all JSON special characters
        // Serializing a string array to JSON cannot fail
        #[allow(clippy::expect_used)]
        let json_array = serde_json::to_string(&serde_json::json!([value_str]))
            .expect("serializing a string array to JSON cannot fail");
        // SQLI-1 fix (FR-6 audit): bind the JSON value as `$1` instead of
        // inlining it inside a `'...'` literal. The gather pipeline renders via
        // `to_string(PostgresQueryBuilder)`, which inlines `Expr::cust`
        // fragments verbatim — `serde_json` does not escape the single quote,
        // so a value like `x') OR true OR ('a` would break out of the literal.
        // `cust_with_values` emits a bound parameter, matching the safe pattern
        // used by `FullTextSearch` / `descendants_exists_expr`. `base_table` and
        // `jsonb_path` are validated identifiers above, so interpolating them is
        // safe.
        let expr = format!("{}.fields->'{}' @> $1::jsonb", ctx.base_table, jsonb_path);

        Ok(Some(Expr::cust_with_values(expr, [json_array])))
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::gather::types::{FilterOperator, FilterValue, QueryFilter};

    fn make_filter(field: &str, value: FilterValue) -> QueryFilter {
        QueryFilter {
            field: field.to_string(),
            operator: FilterOperator::Custom("test".to_string()),
            value,
            exposed: false,
            exposed_label: None,
            widget: Default::default(),
        }
    }

    fn make_context() -> FilterContext {
        FilterContext {
            base_table: "item".to_string(),
            stage_id: crate::models::stage::LIVE_STAGE_ID,
        }
    }

    #[test]
    fn safe_identifier_validation() {
        assert!(is_safe_identifier("category_tag_hierarchy"));
        assert!(is_safe_identifier("tag_id"));
        assert!(is_safe_identifier("_private"));
        assert!(is_safe_identifier("a"));

        assert!(!is_safe_identifier(""));
        assert!(!is_safe_identifier("123abc"));
        assert!(!is_safe_identifier("table; DROP TABLE--"));
        assert!(!is_safe_identifier("foo bar"));
        assert!(!is_safe_identifier("table-name"));
        // Dots are not allowed in identifiers (prevents schema.table injection)
        assert!(!is_safe_identifier("public.evil_table"));
        assert!(!is_safe_identifier("schema.table"));
    }

    #[test]
    fn safe_jsonb_path_validation() {
        assert!(is_safe_jsonb_path("category"));
        assert!(is_safe_jsonb_path("meta.source"));
        assert!(is_safe_jsonb_path("a.b.c"));

        assert!(!is_safe_jsonb_path(""));
        assert!(!is_safe_jsonb_path("field; DROP TABLE--"));
        assert!(!is_safe_jsonb_path(".leading_dot"));
        assert!(!is_safe_jsonb_path("trailing."));
        assert!(!is_safe_jsonb_path("has..double"));
    }

    #[test]
    fn hierarchical_in_rejects_unsafe_field() {
        let handler = HierarchicalInFilterHandler;
        let uuid = Uuid::nil();
        let filter = make_filter("fields.bad;field", FilterValue::Uuid(uuid));
        let config = serde_json::json!({});
        let ctx = make_context();

        let result = handler.build_condition(&filter, &config, &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unsafe JSONB"));
    }

    #[test]
    fn jsonb_array_contains_rejects_unsafe_field() {
        let handler = JsonbArrayContainsFilterHandler;
        let filter = make_filter("fields.bad;field", FilterValue::String("value".to_string()));
        let config = serde_json::json!({});
        let ctx = make_context();

        let result = handler.build_condition(&filter, &config, &ctx);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unsafe JSONB"));
    }

    #[test]
    fn hierarchical_in_build_condition_single_uuid() {
        let handler = HierarchicalInFilterHandler;
        let uuid = Uuid::nil();
        let filter = make_filter("fields.category", FilterValue::Uuid(uuid));
        let config = serde_json::json!({});
        let ctx = make_context();

        let result = handler.build_condition(&filter, &config, &ctx).unwrap();
        assert!(result.is_some());

        let expr = format!("{:?}", result.unwrap());
        assert!(expr.contains("item.fields"));
        assert!(expr.contains("category"));
    }

    #[test]
    fn hierarchical_in_build_condition_multiple_uuids() {
        let handler = HierarchicalInFilterHandler;
        let uuid1 = Uuid::nil();
        let uuid2 = Uuid::from_u128(1);
        let filter = make_filter(
            "fields.tag",
            FilterValue::List(vec![FilterValue::Uuid(uuid1), FilterValue::Uuid(uuid2)]),
        );
        let config = serde_json::json!({});
        let ctx = make_context();

        let result = handler.build_condition(&filter, &config, &ctx).unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn hierarchical_in_build_condition_empty_returns_none() {
        let handler = HierarchicalInFilterHandler;
        let filter = make_filter("fields.tag", FilterValue::List(vec![]));
        let config = serde_json::json!({});
        let ctx = make_context();

        let result = handler.build_condition(&filter, &config, &ctx).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn jsonb_array_contains_build_condition() {
        let handler = JsonbArrayContainsFilterHandler;
        let filter = make_filter("fields.tags", FilterValue::String("rust".to_string()));
        let config = serde_json::json!({});
        let ctx = make_context();

        let result = handler.build_condition(&filter, &config, &ctx).unwrap();
        assert!(result.is_some());

        let expr = format!("{:?}", result.unwrap());
        assert!(expr.contains("item.fields"));
        assert!(expr.contains("tags"));
    }

    #[test]
    fn jsonb_array_contains_escapes_value() {
        let handler = JsonbArrayContainsFilterHandler;
        let filter = make_filter("fields.tags", FilterValue::String(r#"val"ue"#.to_string()));
        let config = serde_json::json!({});
        let ctx = make_context();

        let result = handler.build_condition(&filter, &config, &ctx).unwrap();
        assert!(result.is_some());

        // Verify proper JSON escaping is used (serde_json handles all special chars)
        let expr_debug = format!("{:?}", result.unwrap());
        assert!(expr_debug.contains(r#"[\"val\\\"ue\"]"#) || expr_debug.contains("val"));
    }

    /// SQLI-1 regression (FR-6 audit): a single-quote payload must be rendered
    /// as an escaped, self-contained literal — it must NOT break out of the
    /// string and inject SQL. Renders through `to_string(PostgresQueryBuilder)`,
    /// exactly the path the gather pipeline uses.
    #[test]
    fn jsonb_array_contains_single_quote_is_inert() {
        use sea_query::{Alias, PostgresQueryBuilder, Query};

        let handler = JsonbArrayContainsFilterHandler;
        // Classic break-out-of-literal payload.
        let payload = "x') OR true OR ('a";
        let filter = make_filter("fields.tags", FilterValue::String(payload.to_string()));
        let ctx = make_context();
        let cond = handler
            .build_condition(&filter, &serde_json::json!({}), &ctx)
            .unwrap()
            .expect("condition present");

        let sql = Query::select()
            .expr(Expr::cust("1"))
            .from(Alias::new("item"))
            .and_where(cond)
            .to_string(PostgresQueryBuilder);

        // Safe: SeaQuery renders the bound value as a PostgreSQL `E'...'`
        // escape-string literal with backslash-escaped quotes, so the payload's
        // single quotes (`\'`) stay inside ONE literal instead of breaking out.
        assert!(
            sql.contains("@> E'"),
            "value must render as a bound escape-string literal: {sql}"
        );
        assert!(
            sql.contains(r"\') OR true OR (\'"),
            "single quotes in the payload must be backslash-escaped: {sql}"
        );
        // The vulnerable shape (a bare quote breaking out, which the pre-fix
        // `Expr::cust` inlining produced: `@> '["x') OR ...`) must be absent.
        assert!(
            !sql.contains("@> '[\"x') OR"),
            "single quote must not break out of the literal: {sql}"
        );
    }

    #[test]
    fn jsonb_array_contains_escapes_control_chars() {
        let handler = JsonbArrayContainsFilterHandler;
        // Test with control characters that manual replace() would miss
        let filter = make_filter(
            "fields.tags",
            FilterValue::String("val\nue\ttab".to_string()),
        );
        let config = serde_json::json!({});
        let ctx = make_context();

        let result = handler.build_condition(&filter, &config, &ctx).unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn jsonb_array_contains_no_value_returns_none() {
        let handler = JsonbArrayContainsFilterHandler;
        let filter = make_filter("fields.tags", FilterValue::List(vec![]));
        let config = serde_json::json!({});
        let ctx = make_context();

        let result = handler.build_condition(&filter, &config, &ctx).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn hierarchical_in_equivalence_with_legacy() {
        // Prove that HierarchicalInFilterHandler produces the same IN-clause SQL
        // as the legacy build_category_filter() in query_builder.rs
        let handler = HierarchicalInFilterHandler;
        let uuid = Uuid::nil();
        let filter = make_filter("fields.category", FilterValue::Uuid(uuid));
        let config = serde_json::json!({});
        let ctx = make_context();

        let result = handler.build_condition(&filter, &config, &ctx).unwrap();
        assert!(result.is_some());

        // The legacy code generates: item.fields->>'category' IN ('00000000-0000-0000-0000-000000000000')
        // Our handler generates the same format
        let expected_fragment = format!("item.fields->>'category' IN ('{uuid}')");
        let expr_debug = format!("{:?}", result.unwrap());
        // Both use Expr::cust with the same SQL pattern
        assert!(
            expr_debug.contains(&expected_fragment),
            "expected SQL fragment '{expected_fragment}' in expression debug: {expr_debug}"
        );
    }
}
