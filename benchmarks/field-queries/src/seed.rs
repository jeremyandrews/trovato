//! The fixture: one item type with 200 fields of mixed kinds, its items, and
//! items of four other types, written in SQL so Postgres does the work.
//!
//! Every value is derived from the seed and the row number through
//! `hashint8extended`, so the same options always write the same rows, and a
//! run before a change and a run after it read the same data.

use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, PgPool};

/// The 200 field type.
pub const MAIN_TYPE: &str = "bench_main";
/// The four other types, in the order `m % 4` assigns them.
pub const OTHER_TYPES: [&str; 4] = ["bench_place", "bench_person", "bench_event", "bench_doc"];
/// The tag category the multi value tag fields draw from.
pub const TAG_CATEGORY: &str = "bench_topics";
/// Tags in [`TAG_CATEGORY`].
pub const TAGS: i64 = 500;
/// Members, every item's author one of them.
pub const USERS: i64 = 1000;
/// 2026-01-01, so dates do not move between runs.
pub const NOW: i64 = 1_767_225_600;

/// What a field holds and how it is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Short text, a bare JSON string (some items wrapped as `{"value": …}`).
    Text,
    /// Long text, `{"value": …, "format": …}`.
    LongText,
    /// Integer, a JSON number (some items wrapped).
    Integer,
    /// Decimal, a JSON number.
    Decimal,
    /// Boolean, JSON `true`/`false`.
    Boolean,
    /// Date, `YYYY-MM-DD`.
    Date,
    /// Date and time, ISO 8601 in UTC.
    DateTime,
    /// One reference, an item uuid as a string.
    Reference,
    /// Several references, an array of item uuid strings.
    MultiReference,
    /// Tags, an array of tag uuid strings.
    Tags,
}

/// One field: its name, kind and the SQL that writes its value for row `n`.
#[derive(Debug, Clone)]
pub struct FieldSpec {
    pub name: String,
    pub kind: Kind,
    sql: String,
}

/// A uniform value in `[0, 1)` for row `n` and stream `k`.
fn u(seed: u64, k: u64) -> String {
    format!(
        "((hashint8extended(n, {}) & 2147483647)::float8 / 2147483648.0)",
        seed * 100_000 + k
    )
}

/// A uniform value for row `n`, element `i` of a list, stream `k`.
fn ui(seed: u64, k: u64) -> String {
    format!(
        "((hashint8extended(n * 16 + i, {}) & 2147483647)::float8 / 2147483648.0)",
        seed * 100_000 + k
    )
}

/// The deterministic id of row `expr` of `kind`.
pub fn id_sql(seed: u64, kind: &str, expr: &str) -> String {
    format!("md5('{seed}:{kind}:' || ({expr})::text)::uuid")
}

/// `value` as JSON, or `NULL` (the key is then dropped) for `null_rate` of
/// rows, or wrapped as `{"value": …}` for `wrap_rate` of the others.
fn nullable(seed: u64, k: u64, value: &str, null_rate: f64, wrap_rate: f64) -> String {
    if null_rate <= 0.0 && wrap_rate <= 0.0 {
        return format!("to_jsonb({value})");
    }
    let mut sql = String::from("CASE ");
    if null_rate > 0.0 {
        sql.push_str(&format!(
            "WHEN {} < {null_rate} THEN NULL ",
            u(seed, k + 50_000)
        ));
    }
    if wrap_rate > 0.0 {
        sql.push_str(&format!(
            "WHEN {} < {wrap_rate} THEN jsonb_build_object('value', {value}) ",
            u(seed, k + 60_000)
        ));
    }
    sql.push_str(&format!("ELSE to_jsonb({value}) END"));
    sql
}

/// The 200 fields of [`MAIN_TYPE`].
///
/// The fields the query matrix reads have names that say what they hold; the
/// rest are numbered by kind. The mix: 40 short texts (cardinality 3 to
/// 1,000,000, uniform and skewed, 0 to 60 percent empty), 20 long texts, 30
/// integers, 20 decimals, 20 booleans, 15 dates, 5 date-times, 15 single
/// references, 10 multi value references, 15 multi value tag fields and ten
/// fields that are empty on 95 to 99.5 percent of items.
pub fn main_fields(seed: u64, other_items: i64) -> Vec<FieldSpec> {
    let mut fields = Vec::with_capacity(200);
    let mut k: u64 = 1;
    let mut next = || {
        k += 1;
        k
    };
    let places = (other_items / 4).max(1);
    let people = (other_items / 4).max(1);

    let mut push = |name: String, kind: Kind, sql: String| {
        fields.push(FieldSpec { name, kind, sql });
    };

    // Named short texts.
    let skewed = |seed: u64, k: u64, card: i64, power: f64| {
        format!(
            "('{{}}' || (1 + floor({card} * power({}, {power})))::bigint)",
            u(seed, k)
        )
    };
    let text = |prefix: &str, inner: String| inner.replacen("{}", prefix, 1);
    let kk = next();
    push(
        "ft_status".into(),
        Kind::Text,
        format!(
            "to_jsonb((ARRAY['active','review','draft','archived','legacy'])[1 + floor(5 * power({}, 2))::int])",
            u(seed, kk)
        ),
    );
    let kk = next();
    push(
        "ft_country".into(),
        Kind::Text,
        nullable(seed, kk, &text("c", skewed(seed, kk, 200, 2.5)), 0.02, 0.0),
    );
    let kk = next();
    push(
        "ft_sku".into(),
        Kind::Text,
        "to_jsonb('sku' || lpad(n::text, 9, '0'))".to_string(),
    );
    let _ = kk;
    let kk = next();
    push(
        "ft_color".into(),
        Kind::Text,
        nullable(seed, kk, &text("col", skewed(seed, kk, 12, 1.0)), 0.0, 0.1),
    );
    let kk = next();
    push(
        "ft_name".into(),
        Kind::Text,
        format!(
            "to_jsonb(w[1 + floor(64 * {})::int] || ' ' || w[1 + floor(64 * {})::int] || ' ' || n)",
            u(seed, kk),
            u(seed, kk + 1)
        ),
    );
    // Numbered short texts: cardinality, skew, empty share and wrapped share
    // cycle through realistic combinations.
    let cards = [3_i64, 5, 10, 20, 50, 100, 1_000, 10_000, 100_000, 1_000_000];
    let powers = [1.0, 1.0, 2.0, 3.0];
    let empties = [0.0, 0.05, 0.3, 0.6];
    for i in 1..=35 {
        let kk = next();
        let card = cards[(i - 1) % cards.len()];
        let power = powers[(i - 1) % powers.len()];
        let empty = empties[(i / 3) % empties.len()];
        let wrap = if i % 5 == 0 { 0.1 } else { 0.0 };
        push(
            format!("ft_t{i:02}"),
            Kind::Text,
            nullable(
                seed,
                kk,
                &text("v", skewed(seed, kk, card, power)),
                empty,
                wrap,
            ),
        );
    }

    // Long texts.
    for i in 1..=20 {
        let kk = next();
        push(
            format!("fl_l{i:02}"),
            Kind::LongText,
            format!(
                "CASE WHEN {} < {} THEN NULL ELSE jsonb_build_object('value', \
                 p[1 + floor(8 * {})::int] || ' ' || p[1 + floor(8 * {})::int] || ' ' \
                 || w[1 + floor(64 * {})::int], 'format', 'filtered_html') END",
                u(seed, kk + 50_000),
                if i % 4 == 0 { 0.5 } else { 0.05 },
                u(seed, kk),
                u(seed, kk + 1),
                u(seed, kk + 2)
            ),
        );
    }

    // Named integers.
    let kk = next();
    push(
        "fi_price".into(),
        Kind::Integer,
        nullable(
            seed,
            kk,
            &format!("floor(100000 * power({}, 3))::bigint", u(seed, kk)),
            0.05,
            0.0,
        ),
    );
    let kk = next();
    push(
        "fi_year".into(),
        Kind::Integer,
        nullable(
            seed,
            kk,
            &format!("(1950 + floor(77 * power({}, 0.5)))::int", u(seed, kk)),
            0.0,
            0.0,
        ),
    );
    let kk = next();
    push(
        "fi_rating".into(),
        Kind::Integer,
        nullable(
            seed,
            kk,
            &format!("(1 + floor(5 * {}))::int", u(seed, kk)),
            0.1,
            0.2,
        ),
    );
    let ranges = [100_i64, 1_000, 10_000, 1_000_000, 2_000_000_000];
    for i in 1..=27 {
        let kk = next();
        let max = ranges[(i - 1) % ranges.len()];
        let power = powers[(i - 1) % powers.len()];
        let empty = empties[(i / 4) % empties.len()];
        let wrap = if i % 6 == 0 { 0.1 } else { 0.0 };
        push(
            format!("fi_i{i:02}"),
            Kind::Integer,
            nullable(
                seed,
                kk,
                &format!("floor({max} * power({}, {power}))::bigint", u(seed, kk)),
                empty,
                wrap,
            ),
        );
    }

    // Decimals.
    let kk = next();
    push(
        "fd_score".into(),
        Kind::Decimal,
        nullable(
            seed,
            kk,
            &format!("round((100 * {})::numeric, 2)", u(seed, kk)),
            0.0,
            0.0,
        ),
    );
    for i in 1..=19 {
        let kk = next();
        let max = ranges[(i - 1) % ranges.len()];
        let empty = empties[(i / 4) % empties.len()];
        push(
            format!("fd_d{i:02}"),
            Kind::Decimal,
            nullable(
                seed,
                kk,
                &format!("round(({max} * {})::numeric, 2)", u(seed, kk)),
                empty,
                0.0,
            ),
        );
    }

    // Booleans.
    let kk = next();
    push(
        "fb_featured".into(),
        Kind::Boolean,
        format!("to_jsonb({} < 0.05)", u(seed, kk)),
    );
    let trues = [0.5, 0.2, 0.05, 0.01];
    for i in 1..=19 {
        let kk = next();
        push(
            format!("fb_b{i:02}"),
            Kind::Boolean,
            format!(
                "to_jsonb({} < {})",
                u(seed, kk),
                trues[(i - 1) % trues.len()]
            ),
        );
    }

    // Dates: the published date leans toward recent years.
    let kk = next();
    push(
        "fdt_published".into(),
        Kind::Date,
        format!(
            "to_jsonb(to_char(date '2026-01-01' - floor(5800 * power({}, 2))::int, 'YYYY-MM-DD'))",
            u(seed, kk)
        ),
    );
    for i in 1..=14 {
        let kk = next();
        let empty = empties[(i / 3) % empties.len()];
        push(
            format!("fdt_d{i:02}"),
            Kind::Date,
            nullable(
                seed,
                kk,
                &format!(
                    "to_char(date '2000-01-01' + floor(9500 * {})::int, 'YYYY-MM-DD')",
                    u(seed, kk)
                ),
                empty,
                0.0,
            ),
        );
    }
    let kk = next();
    push(
        "fdt_event".into(),
        Kind::DateTime,
        nullable(
            seed,
            kk,
            &format!(
                "to_char(timestamp '2020-01-01' + floor(220000000 * {}) * interval '1 second', \
                 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
                u(seed, kk)
            ),
            0.1,
            0.0,
        ),
    );
    for i in 1..=4 {
        let kk = next();
        push(
            format!("fdt_t{i:02}"),
            Kind::DateTime,
            nullable(
                seed,
                kk,
                &format!(
                    "to_char(timestamp '2018-01-01' + floor(280000000 * {}) * interval '1 second', \
                     'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
                    u(seed, kk)
                ),
                0.3,
                0.0,
            ),
        );
    }

    // Single references: a place, a person, and others into the whole pool.
    let kk = next();
    push(
        "fr_place".into(),
        Kind::Reference,
        nullable(
            seed,
            kk,
            &format!(
                "{}::text",
                id_sql(
                    seed,
                    "other",
                    &format!(
                        "4 * (1 + floor({places} * power({}, 2)))::bigint",
                        u(seed, kk)
                    )
                )
            ),
            0.05,
            0.0,
        ),
    );
    let kk = next();
    push(
        "fr_owner".into(),
        Kind::Reference,
        format!(
            "to_jsonb({}::text)",
            id_sql(
                seed,
                "other",
                &format!("4 * floor({people} * {})::bigint + 1", u(seed, kk))
            )
        ),
    );
    for i in 1..=13 {
        let kk = next();
        let empty = empties[(i / 3) % empties.len()];
        push(
            format!("fr_r{i:02}"),
            Kind::Reference,
            nullable(
                seed,
                kk,
                &format!(
                    "{}::text",
                    id_sql(
                        seed,
                        "other",
                        &format!("1 + floor({other_items} * {})::bigint", u(seed, kk))
                    )
                ),
                empty,
                0.0,
            ),
        );
    }

    // Multi value references: people, one to four.
    for i in 1..=10 {
        let kk = next();
        let name = if i == 1 {
            "fm_people".to_string()
        } else {
            format!("fm_m{i:02}")
        };
        push(
            name,
            Kind::MultiReference,
            format!(
                "CASE WHEN {} < 0.2 THEN NULL ELSE (SELECT jsonb_agg(DISTINCT {}::text) \
                 FROM generate_series(1, 1 + floor(4 * {})::int) i) END",
                u(seed, kk + 50_000),
                id_sql(
                    seed,
                    "other",
                    &format!(
                        "4 * floor({people} * power({}, 2))::bigint + 1",
                        ui(seed, kk)
                    )
                ),
                u(seed, kk)
            ),
        );
    }

    // Multi value tags: zero to six of 500, skewed toward the first tags.
    for i in 1..=15 {
        let kk = next();
        let name = if i == 1 {
            "fg_topics".to_string()
        } else {
            format!("fg_g{i:02}")
        };
        push(
            name,
            Kind::Tags,
            format!(
                "CASE WHEN {} < 0.15 THEN '[]'::jsonb ELSE (SELECT jsonb_agg(DISTINCT {}::text) \
                 FROM generate_series(1, 1 + floor(6 * {})::int) i) END",
                u(seed, kk + 50_000),
                id_sql(
                    seed,
                    "tag",
                    &format!("1 + floor({TAGS} * power({}, 2.5))::bigint", ui(seed, kk))
                ),
                u(seed, kk)
            ),
        );
    }

    // Sparse fields: present on 0.5 to 5 percent of items.
    let rates = [0.005, 0.01, 0.02, 0.05];
    for i in 1..=10 {
        let kk = next();
        let name = if i == 1 {
            "fx_recall".to_string()
        } else {
            format!("fx_s{i:02}")
        };
        push(
            name,
            Kind::Text,
            format!(
                "CASE WHEN {} < {} THEN to_jsonb('r' || floor(50 * {})::int) ELSE NULL END",
                u(seed, kk + 50_000),
                rates[(i - 1) % rates.len()],
                u(seed, kk)
            ),
        );
    }

    fields
}

/// The SQL `jsonb` expression of every field of `specs`, absent keys dropped.
fn fields_object_sql(specs: &[FieldSpec]) -> String {
    // jsonb_build_object takes at most 100 arguments: 40 fields per call.
    let chunks: Vec<String> = specs
        .chunks(40)
        .map(|chunk| {
            let pairs: Vec<String> = chunk
                .iter()
                .map(|f| format!("'{}', {}", f.name, f.sql))
                .collect();
            format!("jsonb_build_object({})", pairs.join(", "))
        })
        .collect();
    format!("jsonb_strip_nulls({})", chunks.join(" || "))
}

/// The field definition [`ItemType`] settings carry for `spec`.
fn field_definition(spec: &FieldSpec) -> Value {
    use trovato_sdk::types::{FieldDefinition, FieldType};
    let field_type = match spec.kind {
        Kind::Text => FieldType::Text {
            max_length: Some(255),
        },
        Kind::LongText => FieldType::TextLong,
        Kind::Integer => FieldType::Integer,
        Kind::Decimal => FieldType::Float,
        Kind::Boolean => FieldType::Boolean,
        Kind::Date | Kind::DateTime => FieldType::Date,
        Kind::Reference | Kind::MultiReference => FieldType::RecordReference("item".into()),
        Kind::Tags => FieldType::CategoryReference(TAG_CATEGORY.into()),
    };
    let mut def = FieldDefinition::new(&spec.name, field_type);
    def.label = spec.name.replace('_', " ");
    if matches!(spec.kind, Kind::MultiReference | Kind::Tags) {
        def.cardinality = -1;
    }
    serde_json::to_value(def).unwrap_or(Value::Null)
}

/// The fields every other type has.
fn other_fields(seed: u64) -> Vec<FieldSpec> {
    let mut f = Vec::new();
    let mut push = |name: &str, kind: Kind, sql: String| {
        f.push(FieldSpec {
            name: name.into(),
            kind,
            sql,
        });
    };
    push(
        "ft_name",
        Kind::Text,
        format!(
            "to_jsonb(w[1 + floor(64 * {})::int] || ' ' || n)",
            u(seed, 90_001)
        ),
    );
    push(
        "ft_city",
        Kind::Text,
        nullable(
            seed,
            90_002,
            &format!("'city' || floor(500 * power({}, 2))::int", u(seed, 90_002)),
            0.1,
            0.2,
        ),
    );
    push(
        "fi_capacity",
        Kind::Integer,
        format!("to_jsonb(floor(5000 * {})::int)", u(seed, 90_003)),
    );
    push(
        "fdt_date",
        Kind::Date,
        format!(
            "to_jsonb(to_char(date '2010-01-01' + floor(6000 * {})::int, 'YYYY-MM-DD'))",
            u(seed, 90_004)
        ),
    );
    push(
        "fl_body",
        Kind::LongText,
        format!(
            "jsonb_build_object('value', p[1 + floor(8 * {})::int] || ' ' || p[1 + floor(8 * {})::int], \
             'format', 'filtered_html')",
            u(seed, 90_005),
            u(seed, 90_006)
        ),
    );
    push(
        "fg_topics",
        Kind::Tags,
        format!(
            "(SELECT jsonb_agg(DISTINCT {}::text) FROM generate_series(1, 1 + floor(3 * {})::int) i)",
            id_sql(
                seed,
                "tag",
                &format!(
                    "1 + floor({TAGS} * power({}, 2.5))::bigint",
                    ui(seed, 90_007)
                )
            ),
            u(seed, 90_008)
        ),
    );
    push(
        "fb_active",
        Kind::Boolean,
        format!("to_jsonb({} < 0.7)", u(seed, 90_009)),
    );
    f
}

/// Words titles and short texts are made of.
const WORDS: [&str; 64] = [
    "river", "stone", "lantern", "harbor", "meadow", "copper", "willow", "signal", "garden",
    "orbit", "canvas", "thunder", "pepper", "violet", "bridge", "falcon", "marble", "ember",
    "glacier", "orchard", "compass", "velvet", "summit", "pixel", "cedar", "anchor", "prism",
    "saffron", "tundra", "echo", "quarry", "nimbus", "juniper", "basalt", "cobalt", "ripple",
    "sparrow", "timber", "zephyr", "lagoon", "beacon", "cinder", "dune", "fjord", "grove",
    "hollow", "island", "jasper", "kestrel", "linen", "mosaic", "nectar", "onyx", "pebble",
    "quill", "raven", "sable", "thistle", "umber", "valley", "wander", "yarrow", "zenith", "atlas",
];

/// Paragraphs long texts are assembled from, about 140 characters each.
const PARAGRAPHS: [&str; 8] = [
    "A generated record for performance testing. Every word here was chosen by its number, so the same seed always writes the same site again.",
    "Large sites behave differently from small ones: indexes grow, caches miss, and a query that was free at a thousand rows costs real time.",
    "This paragraph exists to give the field some length, so documents carry text of a realistic size and the JSON column is several kilobytes.",
    "Nothing in this site is real. The authors are numbered, the tags are numbered, and the references point at other numbered records too.",
    "Trovato reads this through the same Gather service a real listing uses, which is the point: the benchmark measures the real query path.",
    "Search finds these records by their words. Each title has two words from a fixed list, so a search for one of them matches a fraction.",
    "Fields are filled the way editors fill them: some always, some rarely, some with one of a handful of values and some with a unique one.",
    "The last paragraph of a generated record. The next record is one number further along, and one step older than this one in time order.",
];

fn sql_array(values: &[&str]) -> String {
    let quoted: Vec<String> = values
        .iter()
        .map(|v| format!("'{}'", v.replace('\'', "''")))
        .collect();
    format!("ARRAY[{}]", quoted.join(","))
}

/// Seeding options.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SeedOptions {
    pub main_items: i64,
    pub other_items: i64,
    pub seed: u64,
    pub batch: i64,
    pub parallel: usize,
}

/// The `site_config` key recording what was seeded.
pub const MARKER: &str = "field_bench_fixture";

/// Seed the fixture into `pool`'s (migrated) database.
pub async fn seed(pool: &PgPool, opts: SeedOptions) -> Result<Vec<(String, f64)>> {
    let mut phases = Vec::new();
    let seed = opts.seed;
    let started = Instant::now();

    // Users: one administrator and USERS members.
    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO users (id, name, pass, mail, is_admin, status, created) \
         SELECT {id}, 'bench_user_' || n, 'x', 'bench_user_' || n || '@example.invalid', n = 0, 1, \
                to_timestamp({NOW} - n * 3600) \
         FROM generate_series(0, {USERS}) n ON CONFLICT DO NOTHING",
        id = id_sql(seed, "user", "n")
    )))
    .execute(pool)
    .await
    .context("users")?;

    // Tag category.
    sqlx::query(
        "INSERT INTO category (id, label, description, hierarchy) \
         VALUES ($1, 'Bench topics', 'Benchmark tags', 0) ON CONFLICT DO NOTHING",
    )
    .bind(TAG_CATEGORY)
    .execute(pool)
    .await
    .context("category")?;
    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO category_tag (id, category_id, label, weight, created, changed, slug) \
         SELECT {id}, '{TAG_CATEGORY}', 'Topic ' || n, 0, {NOW}, {NOW}, 'bench-topic-' || n \
         FROM generate_series(1, {TAGS}) n ON CONFLICT DO NOTHING",
        id = id_sql(seed, "tag", "n")
    )))
    .execute(pool)
    .await
    .context("tags")?;
    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO category_tag_hierarchy (tag_id, parent_id) \
         SELECT {id}, NULL FROM generate_series(1, {TAGS}) n ON CONFLICT DO NOTHING",
        id = id_sql(seed, "tag", "n")
    )))
    .execute(pool)
    .await
    .context("tag hierarchy")?;

    // Item types.
    let main = main_fields(seed, opts.other_items);
    let main_defs: Vec<Value> = main.iter().map(field_definition).collect();
    upsert_type(pool, MAIN_TYPE, "Bench record (200 fields)", &main_defs).await?;
    let others = other_fields(seed);
    let other_defs: Vec<Value> = others.iter().map(field_definition).collect();
    for t in OTHER_TYPES {
        upsert_type(pool, t, &t.replace('_', " "), &other_defs).await?;
    }
    phases.push((
        "users, tags, types".to_string(),
        started.elapsed().as_secs_f64(),
    ));

    // Other items: 1..=other_items, type by m % 4, 90 percent published.
    let started = Instant::now();
    let other_sql = item_insert_sql(
        seed,
        "other",
        &format!(
            "(ARRAY['{}','{}','{}','{}'])[1 + (n % 4)::int]",
            OTHER_TYPES[0], OTHER_TYPES[1], OTHER_TYPES[2], OTHER_TYPES[3]
        ),
        &fields_object_sql(&others),
        0.9,
        60,
    );
    run_batches(pool, "other items", opts.other_items, opts, &other_sql).await?;
    phases.push(("other items".to_string(), started.elapsed().as_secs_f64()));

    // Main items.
    let started = Instant::now();
    let main_sql = item_insert_sql(
        seed,
        "main",
        &format!("'{MAIN_TYPE}'"),
        &fields_object_sql(&main),
        0.85,
        30,
    );
    run_batches(pool, "main items", opts.main_items, opts, &main_sql).await?;
    phases.push(("main items".to_string(), started.elapsed().as_secs_f64()));

    let started = Instant::now();
    sqlx::query("VACUUM (ANALYZE) item")
        .execute(pool)
        .await
        .context("vacuum item")?;
    sqlx::query("VACUUM (ANALYZE) item_revision")
        .execute(pool)
        .await
        .context("vacuum item_revision")?;
    sqlx::query("ANALYZE")
        .execute(pool)
        .await
        .context("analyze")?;
    for (key, value) in [
        ("installed", json!(true)),
        ("site_name", json!("Field query benchmark")),
        (MARKER, serde_json::to_value(opts)?),
    ] {
        trovato_kernel::models::SiteConfig::set(pool, key, value).await?;
    }
    phases.push((
        "vacuum and analyze".to_string(),
        started.elapsed().as_secs_f64(),
    ));
    Ok(phases)
}

async fn upsert_type(pool: &PgPool, name: &str, label: &str, defs: &[Value]) -> Result<()> {
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, $2, 'Benchmark fixture', true, 'Title', 'core', $3) \
         ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
    )
    .bind(name)
    .bind(label)
    .bind(json!({ "fields": defs }))
    .execute(pool)
    .await
    .with_context(|| format!("item type {name}"))?;
    Ok(())
}

/// One batch's statement: items `$1..=$2` of `kind`, each with one to three
/// revisions (70, 20 and 10 percent), `published` of them published, created
/// `step` seconds apart, newest first.
fn item_insert_sql(
    seed: u64,
    kind: &str,
    type_sql: &str,
    fields_sql: &str,
    published: f64,
    step: i64,
) -> String {
    let words = sql_array(&WORDS);
    let paragraphs = sql_array(&PARAGRAPHS);
    format!(
        "WITH src AS ( \
           SELECT n, {item_id} AS id, {type_sql} AS type, \
             {author} AS author, \
             initcap(w[1 + (n * 7) % 64]) || ' ' || w[1 + (n * 13 + 5) % 64] || ' ' || n AS title, \
             {NOW}::bigint - n * {step} AS created, \
             CASE WHEN {pub_u} < {published} THEN 1 ELSE 0 END::smallint AS status, \
             CASE WHEN {rev_u} < 0.7 THEN 1 WHEN {rev_u} < 0.9 THEN 2 ELSE 3 END AS revisions, \
             {fields_sql} AS fields \
           FROM generate_series($1::bigint, $2::bigint) n, \
                (SELECT {words} AS w, {paragraphs} AS p) lists \
         ), rev AS ( \
           INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, log) \
           SELECT md5('{seed}:{kind}rev:' || n || ':' || r)::uuid, id, author, title, status, \
                  fields, created - (r - 1) * 86400, 'generated' \
           FROM src, generate_series(1, revisions) r \
         ) \
         INSERT INTO item (id, current_revision_id, type, title, author_id, status, created, \
                           changed, promote, sticky, fields, language) \
         SELECT id, md5('{seed}:{kind}rev:' || n || ':1')::uuid, type, title, author, status, \
                created, created, 0, 0, fields, 'en' \
         FROM src",
        item_id = id_sql(seed, kind, "n"),
        author = id_sql(seed, "user", &format!("1 + n % {USERS}")),
        pub_u = u(seed, 99_001),
        rev_u = u(seed, 99_002),
    )
}

/// Run `sql` over `1..=total` in batches, `opts.parallel` at a time.
async fn run_batches(
    pool: &PgPool,
    what: &str,
    total: i64,
    opts: SeedOptions,
    sql: &str,
) -> Result<()> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};
    let next = Arc::new(AtomicI64::new(1));
    let done = Arc::new(AtomicI64::new(0));
    let started = Instant::now();
    let mut tasks = Vec::new();
    for _ in 0..opts.parallel.max(1) {
        let pool = pool.clone();
        let next = next.clone();
        let done = done.clone();
        let sql = sql.to_string();
        let what = what.to_string();
        tasks.push(tokio::spawn(async move {
            loop {
                let from = next.fetch_add(opts.batch, Ordering::SeqCst);
                if from > total {
                    return Ok::<(), anyhow::Error>(());
                }
                let to = (from + opts.batch - 1).min(total);
                sqlx::query(AssertSqlSafe(sql.clone()))
                    .bind(from)
                    .bind(to)
                    .execute(&pool)
                    .await
                    .with_context(|| format!("{what} {from}..={to}"))?;
                let d = done.fetch_add(to - from + 1, Ordering::SeqCst) + (to - from + 1);
                if d % (opts.batch * 20) < opts.batch {
                    let secs = started.elapsed().as_secs_f64();
                    eprintln!(
                        "  {what}: {d}/{total} ({:.0}/s)",
                        d as f64 / secs.max(0.001)
                    );
                }
            }
        }));
    }
    for t in tasks {
        t.await??;
    }
    Ok(())
}
