//! The query matrix: Gather definitions over the fixture, each run through
//! the real Gather service.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::{Value, json};
use sqlx::PgPool;
use trovato_kernel::gather::{FilterValue, QueryDefinition, QueryFilter};

use crate::seed::{MAIN_TYPE, id_sql};

/// What a case runs.
pub enum Run {
    /// `GatherService::execute_definition` with this page, exposed values and
    /// URL arguments.
    Gather {
        definition: Box<QueryDefinition>,
        page: u32,
        per_page: u32,
        exposed: HashMap<String, FilterValue>,
        url_args: HashMap<String, String>,
    },
    /// `GatherService::fetch_faceted_distinct_values`: the option list of an
    /// exposed filter, narrowed by the other exposed filters' values.
    Facet {
        source_field: String,
        all_exposed: Vec<QueryFilter>,
        exclude_field: String,
        active: HashMap<String, FilterValue>,
    },
}

/// One query of the matrix.
pub struct Case {
    pub id: &'static str,
    pub what: &'static str,
    pub run: Run,
}

fn def(value: Value) -> Box<QueryDefinition> {
    let mut v = value;
    if let Some(obj) = v.as_object_mut() {
        obj.entry("base_table").or_insert(json!("item"));
        obj.entry("item_type").or_insert(json!(MAIN_TYPE));
    }
    #[allow(clippy::expect_used)]
    Box::new(serde_json::from_value(v).expect("matrix definitions are valid"))
}

fn gather(value: Value, page: u32) -> Run {
    Run::Gather {
        definition: def(value),
        page,
        per_page: 10,
        exposed: HashMap::new(),
        url_args: HashMap::new(),
    }
}

fn f(field: &str, operator: &str, value: Value) -> Value {
    json!({"field": field, "operator": operator, "value": value})
}

fn newest() -> Value {
    json!([{"field": "created", "direction": "desc"}])
}

/// The matrix. `seed` names the fixture's ids.
pub async fn cases(pool: &PgPool, seed: u64) -> Result<Vec<Case>> {
    let id = |kind: &str, n: &str| {
        let sql = format!("SELECT {}::text", id_sql(seed, kind, n));
        async move {
            sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(sql))
                .fetch_one(pool)
                .await
        }
    };
    // Tag 3 is common (the tags are skewed toward the first); tag 120 is rare.
    let tag_common = id("tag", "3").await?;
    let tag_rare = id("tag", "120").await?;
    let tag_mid = id("tag", "40").await?;
    // Place 4 * 7 (the place references are skewed toward the first places).
    let place = id("other", "28").await?;
    // Person 4 * 2 + 1.
    let person = id("other", "9").await?;

    let mut cases = vec![
        Case {
            id: "Q01",
            what: "equality on one text field (200 values, skewed), newest first",
            run: gather(
                json!({"filters": [f("fields.ft_country", "equals", json!("c7"))], "sorts": newest()}),
                1,
            ),
        },
        Case {
            id: "Q02",
            what: "IN on one text field (12 values), newest first",
            run: gather(
                json!({"filters": [f("fields.ft_color", "in", json!(["col2", "col5", "col9"]))], "sorts": newest()}),
                1,
            ),
        },
        Case {
            id: "Q03",
            what: "equality on a unique text field (one row)",
            run: gather(
                json!({"filters": [f("fields.ft_sku", "equals", json!("sku000123457"))]}),
                1,
            ),
        },
        Case {
            id: "Q04",
            what: "numeric range on an integer field, newest first",
            run: gather(
                json!({"filters": [
                    f("fields.fi_price", "greater_or_equal", json!(5000)),
                    f("fields.fi_price", "less_or_equal", json!(5100))
                ], "sorts": newest()}),
                1,
            ),
        },
        Case {
            id: "Q05",
            what: "date range, sorted by that date descending, page 1",
            run: gather(
                json!({"filters": [
                    f("fields.fdt_published", "greater_or_equal", json!("2024-01-01")),
                    f("fields.fdt_published", "less_or_equal", json!("2024-12-31"))
                ], "sorts": [{"field": "fields.fdt_published", "direction": "desc"}]}),
                1,
            ),
        },
        Case {
            id: "Q06",
            what: "the same date range and sort, page 500",
            run: gather(
                json!({"filters": [
                    f("fields.fdt_published", "greater_or_equal", json!("2024-01-01")),
                    f("fields.fdt_published", "less_or_equal", json!("2024-12-31"))
                ], "sorts": [{"field": "fields.fdt_published", "direction": "desc"}]}),
                500,
            ),
        },
        Case {
            id: "Q07",
            what: "every published record sorted by a text field",
            run: gather(
                json!({"sorts": [{"field": "fields.ft_name", "direction": "asc"}]}),
                1,
            ),
        },
        Case {
            id: "Q08",
            what: "three fields ANDed (text, boolean, integer range), newest first",
            run: gather(
                json!({"filters": [
                    f("fields.ft_color", "equals", json!("col3")),
                    f("fields.fb_b01", "equals", json!("true")),
                    f("fields.fi_year", "greater_or_equal", json!(2010))
                ], "sorts": newest()}),
                1,
            ),
        },
        Case {
            id: "Q09",
            what: "five fields ANDed, newest first",
            run: gather(
                json!({"filters": [
                    f("fields.ft_status", "equals", json!("active")),
                    f("fields.ft_country", "equals", json!("c2")),
                    f("fields.fb_b01", "equals", json!("true")),
                    f("fields.fi_rating", "equals", json!("5")),
                    f("fields.ft_t01", "equals", json!("v2"))
                ], "sorts": newest()}),
                1,
            ),
        },
        Case {
            id: "Q10",
            what: "three fields ORed, newest first",
            run: gather(
                json!({"filter_groups": [{"conjunction": "or", "filters": [
                    f("fields.ft_country", "equals", json!("c150")),
                    f("fields.ft_t07", "equals", json!("v77")),
                    f("fields.fx_recall", "equals", json!("r7"))
                ]}], "sorts": newest()}),
                1,
            ),
        },
        Case {
            id: "Q11",
            what: "multi value tag containment (a common tag), newest first",
            run: gather(
                json!({"filters": [f("fields.fg_topics", "has_tag", json!(tag_common))], "sorts": newest()}),
                1,
            ),
        },
        Case {
            id: "Q12",
            what: "multi value tag containment (a rare tag), newest first",
            run: gather(
                json!({"filters": [f("fields.fg_topics", "has_tag", json!(tag_rare))], "sorts": newest()}),
                1,
            ),
        },
        Case {
            id: "Q13",
            what: "any of three tags, newest first",
            run: gather(
                json!({"filters": [f("fields.fg_topics", "has_any_tag", json!([tag_rare, tag_mid, tag_common]))], "sorts": newest()}),
                1,
            ),
        },
        Case {
            id: "Q14",
            what: "single reference equality (records at one place), newest first",
            run: gather(
                json!({"filters": [f("fields.fr_place", "equals", json!(place))], "sorts": newest()}),
                1,
            ),
        },
        Case {
            id: "Q15",
            what: "multi value reference containment (records naming one person)",
            run: gather(
                json!({"filters": [f("fields.fm_people", "has_tag", json!(person))], "sorts": newest()}),
                1,
            ),
        },
        Case {
            id: "Q16",
            what: "exposed filter answered by the reader, newest first",
            run: Run::Gather {
                definition: def(json!({"filters": [{
                    "field": "fields.ft_country", "operator": "equals", "value": null,
                    "exposed": true, "exposed_label": "Country"
                }], "sorts": newest()})),
                page: 1,
                per_page: 10,
                exposed: HashMap::from([(
                    "fields.ft_country".to_string(),
                    FilterValue::String("c11".into()),
                )]),
                url_args: HashMap::new(),
            },
        },
        Case {
            id: "Q17",
            what: "contextual filter from a URL argument (records of one owner)",
            run: Run::Gather {
                definition: def(
                    json!({"filters": [f("fields.fr_owner", "equals", json!({"url_arg": "owner"}))], "sorts": newest()}),
                ),
                page: 1,
                per_page: 10,
                exposed: HashMap::new(),
                url_args: HashMap::from([("owner".to_string(), person.clone())]),
            },
        },
        Case {
            id: "Q18",
            what: "facet: the colour options with the country filter applied",
            run: Run::Facet {
                source_field: "fields.ft_color".into(),
                all_exposed: vec![
                    serde_json::from_value(
                        json!({"field": "fields.ft_country", "operator": "equals", "value": null, "exposed": true}),
                    )?,
                    serde_json::from_value(
                        json!({"field": "fields.ft_color", "operator": "equals", "value": null, "exposed": true}),
                    )?,
                ],
                exclude_field: "fields.ft_color".into(),
                active: HashMap::from([(
                    "fields.ft_country".to_string(),
                    FilterValue::String("c3".into()),
                )]),
            },
        },
        Case {
            id: "Q19",
            what: "a field empty on 99.5 percent of records, newest first",
            run: gather(
                json!({"filters": [f("fields.fx_recall", "equals", json!("r7"))], "sorts": newest()}),
                1,
            ),
        },
        Case {
            id: "Q20",
            what: "full text search combined with a field filter",
            run: gather(
                json!({"filters": [
                    f("search_vector", "full_text_search", json!("lantern")),
                    f("fields.ft_color", "equals", json!("col4"))
                ], "sorts": newest()}),
                1,
            ),
        },
        Case {
            id: "Q21",
            what: "a field nobody marked for indexing (ft_t17), newest first",
            run: gather(
                json!({"filters": [f("fields.ft_t17", "equals", json!("v3"))], "sorts": newest()}),
                1,
            ),
        },
        Case {
            id: "Q22",
            what: "boolean equality (5 percent true), newest first",
            run: gather(
                json!({"filters": [f("fields.fb_featured", "equals", json!("true"))], "sorts": newest()}),
                1,
            ),
        },
        Case {
            id: "Q23",
            what: "every published record sorted by an integer field, descending",
            run: gather(
                json!({"sorts": [{"field": "fields.fi_price", "direction": "desc"}]}),
                1,
            ),
        },
        Case {
            id: "Q24",
            what: "wrapped and bare values: integer equality on fi_rating",
            run: gather(
                json!({"filters": [f("fields.fi_rating", "equals", json!("2"))], "sorts": [{"field": "fields.fdt_published", "direction": "desc"}]}),
                1,
            ),
        },
    ];
    cases.sort_by_key(|c| c.id);
    Ok(cases)
}

impl Case {
    /// The values bound to the case's statements that use placeholders, in
    /// order: only a facet has any (its key, the item type, then each active
    /// equality scope's value).
    pub fn bind_values(&self) -> Vec<String> {
        match &self.run {
            Run::Gather { .. } => Vec::new(),
            Run::Facet {
                source_field,
                all_exposed,
                exclude_field,
                active,
            } => {
                let mut values = vec![
                    source_field
                        .strip_prefix("fields.")
                        .unwrap_or(source_field)
                        .to_string(),
                    MAIN_TYPE.to_string(),
                ];
                for filter in all_exposed {
                    if filter.field == *exclude_field {
                        continue;
                    }
                    if let Some(v) = active.get(&filter.field).and_then(FilterValue::as_string) {
                        values.push(v);
                    }
                }
                values
            }
        }
    }
}
