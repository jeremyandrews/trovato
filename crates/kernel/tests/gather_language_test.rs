//! B80.3: Gather's boolean grouping and aggregates, as SQL.
//!
//! Golden SQL for `filter_groups`, `group_by`, `aggregates`, `having` and
//! `distinct`, and the validator's rules for them
//! (`docs/design/plugin-query-surface.md` section 7). The definitions are the
//! JSON a plugin or a config file writes, so a key the kernel does not read
//! shows up here as SQL that lacks it. No database: the builder is pure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};
use trovato_kernel::gather::{GatherQueryBuilder, GatherService, QueryDefinition};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;

fn def(json: Value) -> QueryDefinition {
    serde_json::from_value(json).expect("definition parses")
}

fn sql(json: Value) -> String {
    GatherQueryBuilder::new(def(json), LIVE_STAGE_ID)
        .with_viewer(Some(UserContext::anonymous()))
        .build(1, 10)
}

fn count_sql(json: Value) -> String {
    GatherQueryBuilder::new(def(json), LIVE_STAGE_ID)
        .with_viewer(Some(UserContext::anonymous()))
        .build_count()
}

fn errors(json: Value) -> Vec<String> {
    GatherService::validate_definition(&def(json))
}

fn eq(field: &str, value: &str) -> Value {
    json!({"field": field, "operator": "equals", "value": value})
}

// =============================================================================
// Boolean grouping
// =============================================================================

/// An `or` group is parenthesised and ANDed with the top-level filters, the
/// access predicate and the stage.
#[test]
fn an_or_group_is_anded_with_the_rest() {
    let sql = sql(json!({
        "base_table": "item",
        "filters": [eq("type", "blog")],
        "filter_groups": [{
            "conjunction": "or",
            "filters": [eq("fields.city", "Barga"), eq("fields.city", "Lucca")]
        }]
    }));
    assert_eq!(
        sql,
        "SELECT \"item\".* FROM \"item\" WHERE \"item\".\"type\" = 'blog' \
         AND ((COALESCE(item.fields->'city'->>'value', item.fields->>'city')) = 'Barga' \
         OR (COALESCE(item.fields->'city'->>'value', item.fields->>'city')) = 'Lucca') \
         AND \"item\".\"status\" = 1 \
         AND \"item\".\"stage_id\" = '0193a5a0-0000-7000-8000-000000000001' \
         LIMIT 10 OFFSET 0"
    );
}

/// Groups nest: `a AND (b OR (c AND d))`.
#[test]
fn groups_nest() {
    let sql = sql(json!({
        "base_table": "item",
        "item_type": "blog",
        "filter_groups": [{
            "conjunction": "or",
            "filters": [eq("fields.city", "Barga")],
            "groups": [{
                "conjunction": "and",
                "filters": [eq("fields.city", "Lucca"), eq("fields.lang", "it")]
            }]
        }]
    }));
    assert_eq!(
        sql,
        "SELECT \"item\".* FROM \"item\" WHERE \
         ((COALESCE(item.fields->'city'->>'value', item.fields->>'city')) = 'Barga' \
         OR ((COALESCE(item.fields->'city'->>'value', item.fields->>'city')) = 'Lucca' \
         AND (COALESCE(item.fields->'lang'->>'value', item.fields->>'lang')) = 'it')) \
         AND \"item\".\"status\" = 1 \
         AND \"item\".\"stage_id\" = '0193a5a0-0000-7000-8000-000000000001' \
         AND \"item\".\"type\" = 'blog' LIMIT 10 OFFSET 0"
    );
}

/// A member that places no constraint (here an empty `in`) is true: an `or`
/// group holding one places none, and an `and` group drops it.
#[test]
fn an_unconstrained_member_is_true() {
    let empty_in = json!({"field": "type", "operator": "in", "value": []});
    let or = sql(json!({
        "base_table": "item",
        "item_type": "blog",
        "filter_groups": [{"conjunction": "or", "filters": [eq("fields.city", "Barga"), empty_in]}]
    }));
    assert!(!or.contains("Barga"), "{or}");

    let and = sql(json!({
        "base_table": "item",
        "item_type": "blog",
        "filter_groups": [{"conjunction": "and", "filters": [eq("fields.city", "Barga"), empty_in]}]
    }));
    assert!(
        and.contains("(COALESCE(item.fields->'city'->>'value', item.fields->>'city')) = 'Barga'"),
        "{and}"
    );
}

/// The count carries the group too, so the pager total matches the rows.
#[test]
fn the_count_carries_the_group() {
    let count = count_sql(json!({
        "base_table": "item",
        "filter_groups": [{"conjunction": "or", "filters": [eq("type", "blog"), eq("type", "news")]}]
    }));
    assert_eq!(
        count,
        "SELECT COUNT(*) FROM \"item\" WHERE (\"item\".\"type\" = 'blog' OR \"item\".\"type\" = 'news') \
         AND \"item\".\"status\" = 1 \
         AND \"item\".\"stage_id\" = '0193a5a0-0000-7000-8000-000000000001'"
    );
}

/// Four levels deep is one too many; an exposed or semantic filter inside a
/// group is refused, since both are resolved for the top level only.
#[test]
fn the_validator_bounds_groups() {
    let deep = json!({
        "base_table": "item",
        "filter_groups": [{"groups": [{"groups": [{"groups": [{"filters": [eq("type", "blog")]}]}]}]}]
    });
    assert!(
        errors(deep).iter().any(|e| e.contains("nest 4 deep")),
        "a fourth level is refused"
    );

    let three = json!({
        "base_table": "item",
        "filter_groups": [{"groups": [{"groups": [{"filters": [eq("type", "blog")]}]}]}]
    });
    assert!(errors(three).is_empty(), "three levels are allowed");

    let exposed = json!({
        "base_table": "item",
        "filter_groups": [{"filters": [{"field": "type", "operator": "equals", "value": "", "exposed": true}]}]
    });
    assert!(
        errors(exposed)
            .iter()
            .any(|e| e.contains("cannot be exposed")),
        "exposed in a group"
    );

    let semantic = json!({
        "base_table": "item",
        "filter_groups": [{"filters": [{"field": "title", "operator": "semantic_similarity", "value": "x"}]}]
    });
    assert!(
        errors(semantic)
            .iter()
            .any(|e| e.contains("cannot be semantic_similarity")),
        "semantic in a group"
    );

    let unsafe_field = json!({
        "base_table": "item",
        "filter_groups": [{"filters": [eq("password_hash", "x")]}]
    });
    assert!(
        errors(unsafe_field)
            .iter()
            .any(|e| e.contains("may not be read")),
        "a grouped filter is held to the same column rules"
    );
}

// =============================================================================
// Aggregates
// =============================================================================

fn votes_per_item() -> Value {
    json!({
        "base_table": "b80_votes",
        "stage_aware": false,
        "filters": [{"field": "score", "operator": "greater_than", "value": 0}],
        "group_by": ["item_id"],
        "aggregates": [
            {"function": "count", "label": "votes"},
            {"function": "sum", "field": "score", "label": "total"},
            {"function": "avg", "field": "score", "label": "mean"}
        ],
        "having": [{"field": "votes", "operator": "greater_or_equal", "value": 2}],
        "sorts": [{"field": "total", "direction": "desc"}, {"field": "item_id", "direction": "asc"}]
    })
}

/// GROUP BY with aggregates, HAVING on an aggregate (as the expression, which
/// PostgreSQL requires) and ORDER BY a label (as the alias).
#[test]
fn group_by_with_aggregates_having_and_a_sort_on_a_label() {
    assert_eq!(
        sql(votes_per_item()),
        "SELECT \"b80_votes\".\"item_id\", COUNT(*) AS \"votes\", \
         SUM(\"b80_votes\".\"score\") AS \"total\", AVG(\"b80_votes\".\"score\") AS \"mean\" \
         FROM \"b80_votes\" WHERE \"b80_votes\".\"score\" > 0 \
         GROUP BY \"b80_votes\".\"item_id\" HAVING COUNT(*) >= 2 \
         ORDER BY \"total\" DESC, \"b80_votes\".\"item_id\" ASC LIMIT 10 OFFSET 0"
    );
    assert!(errors(votes_per_item()).is_empty());
}

/// A grouped count counts groups, not base rows.
#[test]
fn a_grouped_count_counts_groups() {
    assert_eq!(
        count_sql(votes_per_item()),
        "SELECT COUNT(*) FROM (SELECT \"b80_votes\".\"item_id\", COUNT(*) AS \"votes\", \
         SUM(\"b80_votes\".\"score\") AS \"total\", AVG(\"b80_votes\".\"score\") AS \"mean\" \
         FROM \"b80_votes\" WHERE \"b80_votes\".\"score\" > 0 \
         GROUP BY \"b80_votes\".\"item_id\" HAVING COUNT(*) >= 2) AS \"_g\""
    );
}

/// `min`, `max` and a counted column, with no `group_by`: one summary row.
#[test]
fn aggregates_without_group_by_are_one_row() {
    let sql = sql(json!({
        "base_table": "b80_votes",
        "stage_aware": false,
        "aggregates": [
            {"function": "min", "field": "score", "label": "low"},
            {"function": "max", "field": "score", "label": "high"},
            {"function": "count", "field": "item_id", "label": "rated"}
        ]
    }));
    assert_eq!(
        sql,
        "SELECT MIN(\"b80_votes\".\"score\") AS \"low\", MAX(\"b80_votes\".\"score\") AS \"high\", \
         COUNT(\"b80_votes\".\"item_id\") AS \"rated\" FROM \"b80_votes\" LIMIT 10 OFFSET 0"
    );
}

/// `distinct` on a projection, and its count.
#[test]
fn distinct_projects_distinct_rows() {
    let definition = json!({
        "base_table": "b80_votes",
        "stage_aware": false,
        "fields": [{"field_name": "item_id"}],
        "distinct": true
    });
    assert_eq!(
        sql(definition.clone()),
        "SELECT DISTINCT \"b80_votes\".\"item_id\" FROM \"b80_votes\" LIMIT 10 OFFSET 0"
    );
    assert_eq!(
        count_sql(definition),
        "SELECT COUNT(*) FROM (SELECT DISTINCT \"b80_votes\".\"item_id\" FROM \"b80_votes\") AS \"_g\""
    );
}

/// An aggregate on `item` or a record type would count rows the viewer may
/// not see, so the validator refuses every summary clause there.
#[test]
fn summaries_are_refused_on_content() {
    for clause in [
        json!({"group_by": ["type"]}),
        json!({"aggregates": [{"function": "count", "label": "n"}]}),
        json!({"distinct": true}),
    ] {
        let mut item = json!({"base_table": "item"});
        let mut record = json!({"record_type": "b80_record"});
        for (k, v) in clause.as_object().unwrap() {
            item[k] = v.clone();
            record[k] = v.clone();
        }
        assert!(
            errors(item.clone())
                .iter()
                .any(|e| e.contains("not available on content")),
            "item: {item}"
        );
        assert!(
            errors(record.clone())
                .iter()
                .any(|e| e.contains("not available on content")),
            "record: {record}"
        );
    }
}

/// The shapes the validator refuses on a non-content base.
#[test]
fn the_validator_holds_aggregates_to_their_shape() {
    let base = || json!({"base_table": "b80_votes", "stage_aware": false});
    let refused = |patch: Value, why: &str| {
        let mut d = base();
        for (k, v) in patch.as_object().unwrap() {
            d[k] = v.clone();
        }
        let errs = errors(d.clone());
        assert!(errs.iter().any(|e| e.contains(why)), "{d} -> {errs:?}");
    };

    refused(json!({"group_by": ["fields.city"]}), "not a plain column");
    refused(json!({"group_by": ["password_hash"]}), "not a plain column");
    refused(
        json!({"aggregates": [{"function": "sum", "label": "s"}]}),
        "needs a field",
    );
    refused(
        json!({"aggregates": [{"function": "count", "label": "n"}, {"function": "count", "label": "n"}]}),
        "used twice",
    );
    refused(
        json!({"aggregates": [{"function": "count", "label": "bad label"}]}),
        "invalid characters",
    );
    refused(
        json!({"aggregates": [{"function": "count", "label": "n"}],
               "having": [{"field": "score", "operator": "greater_than", "value": 1}]}),
        "not an aggregate label",
    );
    refused(
        json!({"aggregates": [{"function": "count", "label": "n"}],
               "having": [{"field": "n", "operator": "contains", "value": "1"}]}),
        "compare it with a number",
    );
    refused(
        json!({"group_by": ["item_id"], "fields": [{"field_name": "score"}]}),
        "sets no fields",
    );
    refused(
        json!({"group_by": ["item_id"], "sorts": [{"field": "score", "direction": "asc"}]}),
        "neither a group_by column nor an aggregate label",
    );
}

/// A definition with none of the new keys emits what it always did: the
/// existing golden strings in `query_builder.rs` hold, and so does this one
/// on a non-item base.
#[test]
fn a_plain_definition_is_unchanged() {
    assert_eq!(
        sql(json!({"base_table": "b80_votes", "stage_aware": false,
                   "sorts": [{"field": "score", "direction": "desc"}]})),
        "SELECT \"b80_votes\".* FROM \"b80_votes\" ORDER BY \"b80_votes\".\"score\" DESC LIMIT 10 OFFSET 0"
    );
    assert_eq!(
        count_sql(json!({"base_table": "b80_votes", "stage_aware": false})),
        "SELECT COUNT(*) FROM \"b80_votes\""
    );
}
