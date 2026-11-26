//! Item field indexes (B207): every Gather filter, sort, exposed and
//! contextual filter and facet on a marked field is answered through that
//! field's index, and the "Filterable and sortable" setting builds and drops
//! the index.
//!
//! Each test seeds its own scratch database (never the shared test database)
//! with 12,000 items of one type, runs the real `GatherService`, captures the
//! statements it sends, and asks Postgres how it would run each one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};
use sqlx::{AssertSqlSafe, PgPool};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use trovato_kernel::content::ContentTypeRegistry;
use trovato_kernel::gather::field_index;
use trovato_kernel::gather::{
    CategoryService, FilterValue, GatherConfig, GatherExtensionRegistry, GatherService,
    QueryContext, QueryDefinition, QueryDisplay, QueryFilter,
};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_test_utils::scratch_db::ScratchDb;

const TYPE: &str = "fx_rec";
/// Enough items that an index beats reading the type for every shape, on
/// PostgreSQL 16 and 17, and few enough that each test, which seeds its own
/// database and fills and builds every index, stays well inside the CI budget.
const ITEMS: i64 = 12_000;

// ---------------------------------------------------------------------------
// Statement capture
// ---------------------------------------------------------------------------

tokio::task_local! {
    static CAPTURED: Arc<Mutex<Vec<String>>>;
}

struct Capture;

#[derive(Default)]
struct Text {
    summary: Option<String>,
    full: Option<String>,
}

impl Visit for Text {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "summary" => self.summary = Some(value.to_owned()),
            "db.statement" if !value.trim().is_empty() => self.full = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

impl<S: Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &Event<'_>, _ctx: LayerContext<'_, S>) {
        if event.metadata().target() != "sqlx::query" {
            return;
        }
        let _ = CAPTURED.try_with(|captured| {
            let mut text = Text::default();
            event.record(&mut text);
            captured
                .lock()
                .unwrap()
                .push(text.full.or(text.summary).unwrap_or_default());
        });
    }
}

fn install_capture() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let subscriber = tracing_subscriber::registry().with(
            Capture.with_filter(
                tracing_subscriber::filter::Targets::new()
                    .with_target("sqlx::query", tracing::Level::DEBUG),
            ),
        );
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

/// Run `f`, returning its output and every statement it sent.
async fn captured<T>(f: impl std::future::Future<Output = T>) -> (T, Vec<String>) {
    let statements = Arc::new(Mutex::new(Vec::new()));
    let out = CAPTURED.scope(statements.clone(), f).await;
    let list = statements.lock().unwrap().clone();
    (out, list)
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Site {
    _db: ScratchDb,
    pool: PgPool,
    types: Arc<ContentTypeRegistry>,
    gather: Arc<GatherService>,
}

fn field(name: &str, field_type: Value, cardinality: i32, filterable: Option<bool>) -> Value {
    let mut settings = serde_json::Map::new();
    if let Some(f) = filterable {
        settings.insert("filterable".into(), json!(f));
    }
    json!({
        "field_name": name, "field_type": field_type, "label": name,
        "required": false, "cardinality": cardinality, "settings": settings,
    })
}

fn fields_setting(note_filterable: Option<bool>) -> Value {
    json!({"fields": [
        field("city", json!({"Text": {"max_length": 255}}), 1, Some(true)),
        field("price", json!("Integer"), 1, Some(true)),
        field("published_on", json!("Date"), 1, Some(true)),
        field("tags", json!({"CategoryReference": "fx_topics"}), -1, Some(true)),
        field("owner", json!({"RecordReference": "item"}), 1, Some(true)),
        field("active", json!("Boolean"), 1, Some(true)),
        field("note", json!({"Text": {"max_length": 255}}), 1, note_filterable),
        field("body", json!("TextLong"), 1, Some(true)),
    ]})
}

async fn site(label: &str) -> Site {
    site_with_ttl(label, Duration::from_secs(60)).await
}

/// A site whose content type cache keeps an entry for `type_ttl`.
async fn site_with_ttl(label: &str, type_ttl: Duration) -> Site {
    install_capture();
    let db = ScratchDb::create(label).await;
    let pool = db.pool().clone();
    trovato_kernel::db::run_migrations(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'Fx record', '', true, 'Title', 'core', $2)",
    )
    .bind(TYPE)
    .bind(fields_setting(Some(false)))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO category (id, label, hierarchy) VALUES ('fx_topics', 'Fx topics', 0)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO category_tag (id, category_id, label, weight, created, changed, slug) \
         SELECT md5('tag' || n)::uuid, 'fx_topics', 'T' || n, 0, 0, 0, 'fx-t-' || n \
         FROM generate_series(1, 50) n",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO category_tag_hierarchy (tag_id, parent_id) \
         SELECT md5('tag' || n)::uuid, NULL FROM generate_series(1, 50) n",
    )
    .execute(&pool)
    .await
    .unwrap();
    // City wrapped as {"value": …} on every fifth item; price missing on
    // every eleventh and a numeric string on every thirteenth; a long body so
    // the documents are stored out of line, as real ones are.
    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO item (id, type, title, author_id, status, created, changed, fields, language) \
         SELECT md5('item' || n)::uuid, '{TYPE}', 'Item ' || n, '00000000-0000-0000-0000-000000000000', \
                CASE WHEN n % 7 = 0 THEN 0 ELSE 1 END, 1700000000 + n, 1700000000 + n, \
                jsonb_strip_nulls(jsonb_build_object( \
                  'city', CASE WHEN n % 5 = 0 THEN jsonb_build_object('value', 'city' || (n % 40)) \
                               ELSE to_jsonb('city' || (n % 40)) END, \
                  'price', CASE WHEN n % 11 = 0 THEN NULL \
                                WHEN n % 13 = 0 THEN to_jsonb((n % 1000)::text) \
                                ELSE to_jsonb(n % 1000) END, \
                  'published_on', to_jsonb(to_char(date '2020-01-01' + (n % 2000), 'YYYY-MM-DD')), \
                  'tags', jsonb_build_array(md5('tag' || (1 + n % 50))::uuid::text, \
                                            md5('tag' || (1 + (n * 7) % 50))::uuid::text), \
                  'owner', to_jsonb(md5('owner' || (n % 300))::uuid::text), \
                  'active', to_jsonb(n % 10 = 0), \
                  'note', to_jsonb('n' || (n % 20)), \
                  'body', jsonb_build_object('value', repeat(md5(n::text), 60), 'format', 'plain_text') \
                )), 'en' \
         FROM generate_series(1, {ITEMS}) n"
    )))
    .execute(&pool)
    .await
    .unwrap();
    // A translation of some items, for the language overlay.
    let translation =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../plugins/trovato_content_translation/migrations/001_create_item_translation.sql",
        ))
        .unwrap();
    sqlx::raw_sql(AssertSqlSafe(translation))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO item_translation (item_id, language, title, fields) \
         SELECT id, 'de', 'Artikel', jsonb_build_object('city', 'stadt') FROM item \
         WHERE type = 'fx_rec' AND created % 3 = 0",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("ANALYZE").execute(&pool).await.unwrap();

    let types = Arc::new(ContentTypeRegistry::new(
        pool.clone(),
        type_ttl,
        Default::default(),
    ));
    types.reload_from_db().await.unwrap();
    let categories =
        CategoryService::new(pool.clone(), Duration::from_secs(60), Default::default());
    let gather = GatherService::new(
        pool.clone(),
        categories,
        Arc::new(GatherExtensionRegistry::new()),
        GatherConfig {
            ttl: Duration::from_secs(60),
            max_page_size: 100,
            access: Default::default(),
        },
        None,
        None,
        Default::default(),
    );
    gather.set_content_types(types.clone());
    Site {
        _db: db,
        pool,
        types,
        gather,
    }
}

impl Site {
    async fn build_indexes(&self) {
        field_index::reconcile_with(&self.pool, &self.types.list(), &HashSet::new())
            .await
            .unwrap();
        field_index::drain(&self.pool).await.unwrap();
        self.gather.refresh_served().await;
    }

    async fn set_note_filterable(&self, value: Option<bool>) {
        sqlx::query("UPDATE item_type SET settings = $2 WHERE type = $1")
            .bind(TYPE)
            .bind(fields_setting(value))
            .execute(&self.pool)
            .await
            .unwrap();
        self.types.reload_from_db().await.unwrap();
    }

    async fn indexes(&self) -> HashSet<String> {
        sqlx::query_scalar::<_, String>(
            "SELECT indexname::text FROM pg_indexes WHERE tablename = 'item' AND indexname LIKE 'item_f%'",
        )
        .fetch_all(&self.pool)
        .await
        .unwrap()
        .into_iter()
        .collect()
    }
}

fn definition(v: Value) -> QueryDefinition {
    let mut v = v;
    let obj = v.as_object_mut().unwrap();
    obj.entry("base_table").or_insert(json!("item"));
    obj.entry("item_type").or_insert(json!(TYPE));
    serde_json::from_value(v).unwrap()
}

fn f(field: &str, op: &str, value: Value) -> Value {
    json!({"field": field, "operator": op, "value": value})
}

fn newest() -> Value {
    json!([{"field": "created", "direction": "desc"}])
}

/// What one shape runs: a gather (definition, page, exposed values, URL
/// arguments, language) or a facet.
enum Shape {
    Gather {
        def: Box<QueryDefinition>,
        page: u32,
        exposed: HashMap<String, FilterValue>,
        url_args: HashMap<String, String>,
        language: Option<String>,
    },
    Facet {
        source: String,
        exposed: Vec<QueryFilter>,
        exclude: String,
        active: HashMap<String, FilterValue>,
    },
}

fn gather(def: Value) -> Shape {
    Shape::Gather {
        def: Box::new(definition(def)),
        page: 1,
        exposed: HashMap::new(),
        url_args: HashMap::new(),
        language: None,
    }
}

async fn shapes(pool: &PgPool) -> Vec<(&'static str, Shape)> {
    let tag_id: String = sqlx::query_scalar("SELECT md5('tag3')::uuid::text")
        .fetch_one(pool)
        .await
        .unwrap();
    let tag_id2: String = sqlx::query_scalar("SELECT md5('tag9')::uuid::text")
        .fetch_one(pool)
        .await
        .unwrap();
    let owner: String = sqlx::query_scalar("SELECT md5('owner17')::uuid::text")
        .fetch_one(pool)
        .await
        .unwrap();
    vec![
        ("equality", gather(json!({"filters": [f("fields.city", "equals", json!("city7"))], "sorts": newest()}))),
        ("in", gather(json!({"filters": [f("fields.city", "in", json!(["city1", "city2"]))], "sorts": newest()}))),
        ("numeric range", gather(json!({"filters": [
            f("fields.price", "greater_or_equal", json!(100)),
            f("fields.price", "less_or_equal", json!(110))], "sorts": newest()}))),
        ("number equality given as text", gather(json!({"filters": [f("fields.price", "equals", json!("250"))], "sorts": newest()}))),
        ("date range sorted by the date", gather(json!({"filters": [
            f("fields.published_on", "greater_or_equal", json!("2024-01-01")),
            f("fields.published_on", "less_or_equal", json!("2024-03-31"))],
            "sorts": [{"field": "fields.published_on", "direction": "desc"}, {"field": "created", "direction": "desc"}]}))),
        // Page 500 of a range most items match, as page 500 is at scale: a
        // range that leaves only a few more items than the offset skips makes
        // reading the whole type and sorting it a fair plan, and PostgreSQL
        // 16 takes it.
        ("date range, page 500", Shape::Gather {
            def: Box::new(definition(json!({"filters": [f("fields.published_on", "greater_or_equal", json!("2021-01-01"))],
                "sorts": [{"field": "fields.published_on", "direction": "desc"}, {"field": "created", "direction": "desc"}]}))),
            page: 500,
            exposed: HashMap::new(),
            url_args: HashMap::new(),
            language: None,
        }),
        ("sort on a text field", gather(json!({"sorts": [{"field": "fields.city", "direction": "asc"}, {"field": "created", "direction": "asc"}]}))),
        ("sort on a number field", gather(json!({"sorts": [{"field": "fields.price", "direction": "desc"}, {"field": "created", "direction": "desc"}]}))),
        ("three fields ANDed", gather(json!({"filters": [
            f("fields.city", "equals", json!("city10")),
            f("fields.active", "equals", json!("true")),
            f("fields.price", "greater_than", json!(500))], "sorts": newest()}))),
        ("three fields ORed", gather(json!({"filter_groups": [{"conjunction": "or", "filters": [
            f("fields.city", "equals", json!("city9")),
            f("fields.owner", "equals", json!(owner.clone())),
            f("fields.price", "equals", json!(7))]}], "sorts": newest()}))),
        ("tag containment", gather(json!({"filters": [f("fields.tags", "has_tag", json!(tag_id.clone()))], "sorts": newest()}))),
        ("any of two tags", gather(json!({"filters": [f("fields.tags", "has_any_tag", json!([tag_id.clone(), tag_id2.clone()]))], "sorts": newest()}))),
        ("reference", gather(json!({"filters": [f("fields.owner", "equals", json!(owner.clone()))], "sorts": newest()}))),
        ("exposed filter", Shape::Gather {
            def: Box::new(definition(json!({"filters": [{"field": "fields.city", "operator": "equals", "value": null, "exposed": true}], "sorts": newest()}))),
            page: 1,
            exposed: HashMap::from([("fields.city".to_string(), FilterValue::String("city11".into()))]),
            url_args: HashMap::new(),
            language: None,
        }),
        ("contextual filter", Shape::Gather {
            def: Box::new(definition(json!({"filters": [f("fields.owner", "equals", json!({"url_arg": "owner"}))], "sorts": newest()}))),
            page: 1,
            exposed: HashMap::new(),
            url_args: HashMap::from([("owner".to_string(), owner.clone())]),
            language: None,
        }),
        ("translated listing", Shape::Gather {
            def: Box::new(definition(json!({"filters": [f("fields.city", "equals", json!("city5"))], "sorts": newest()}))),
            page: 1,
            exposed: HashMap::new(),
            url_args: HashMap::new(),
            language: Some("de".into()),
        }),
        ("facet with a filter applied", Shape::Facet {
            source: "fields.city".into(),
            exposed: vec![
                serde_json::from_value(json!({"field": "fields.owner", "operator": "equals", "value": null, "exposed": true})).unwrap(),
                serde_json::from_value(json!({"field": "fields.city", "operator": "equals", "value": null, "exposed": true})).unwrap(),
            ],
            exclude: "fields.city".into(),
            active: HashMap::from([("fields.owner".to_string(), FilterValue::String(owner.clone()))]),
        }),
    ]
}

/// Run one shape; return the ids (or facet values) it answered with and the
/// statements it sent.
async fn run(site: &Site, shape: &Shape) -> (Vec<String>, Vec<String>) {
    let gather = site.gather.clone();
    let (out, statements) = captured(async {
        match shape {
            Shape::Gather {
                def,
                page,
                exposed,
                url_args,
                language,
            } => {
                let context = QueryContext {
                    url_args: url_args.clone(),
                    language: language.clone(),
                    ..QueryContext::default()
                };
                let display = QueryDisplay {
                    items_per_page: Some(10),
                    ..QueryDisplay::default()
                };
                let result = gather
                    .execute_definition(
                        def,
                        &display,
                        *page,
                        exposed.clone(),
                        LIVE_STAGE_ID,
                        &context,
                    )
                    .await
                    .unwrap();
                result
                    .items
                    .iter()
                    .map(|r| r["id"].as_str().unwrap_or_default().to_string())
                    .collect::<Vec<_>>()
            }
            Shape::Facet {
                source,
                exposed,
                exclude,
                active,
            } => gather
                .fetch_faceted_distinct_values(source, TYPE, exposed, exclude, active)
                .await
                .unwrap(),
        }
    })
    .await;
    (out, statements)
}

/// The plan of every statement that reads `item`, as JSON.
async fn plans(pool: &PgPool, statements: &[String]) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for sql in statements {
        let lower = sql.to_ascii_lowercase();
        if !(lower.starts_with("select") || lower.starts_with("with"))
            || !(lower.contains("from \"item\"") || lower.contains("from item"))
        {
            continue;
        }
        // A statement with parameters is planned as PostgreSQL plans it
        // before it knows them (GENERIC_PLAN, PostgreSQL 16 and later): the
        // plan a cached statement keeps.
        let options = if sql.contains("$1") {
            "FORMAT JSON, GENERIC_PLAN"
        } else {
            "FORMAT JSON"
        };
        // The simple protocol (raw_sql): the extended one would want the
        // parameters bound.
        let row = sqlx::raw_sql(AssertSqlSafe(format!("EXPLAIN ({options}) {sql}")))
            .fetch_one(pool)
            .await
            .unwrap();
        let plan: Value = sqlx::Row::get(&row, 0);
        out.push((sql.clone(), plan));
    }
    out
}

/// Every scan step of a plan: (step type, relation, index).
fn scans(plan: &Value, out: &mut Vec<(String, Option<String>, Option<String>)>) {
    if let Some(kind) = plan["Node Type"].as_str()
        && kind.contains("Scan")
    {
        out.push((
            kind.to_string(),
            plan["Relation Name"].as_str().map(str::to_string),
            plan["Index Name"].as_str().map(str::to_string),
        ));
    }
    if let Some(children) = plan["Plans"].as_array() {
        for child in children {
            scans(child, out);
        }
    }
    if let Some(root) = plan.get(0) {
        scans(&root["Plan"], out);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Every filter, sort, exposed and contextual filter and facet shape on a
/// marked field is planned through that field's index: no statement reads
/// `item` with a sequential scan, and each uses a field index (`item_fx_`, or the type's `item_fv_` GIN).
#[tokio::test]
async fn planner_uses_the_field_index_for_every_shape() {
    let site = site("field_index_plan").await;
    site.build_indexes().await;
    let mut failures = Vec::new();
    for (name, shape) in shapes(&site.pool).await {
        let (_, statements) = run(&site, &shape).await;
        let plans = plans(&site.pool, &statements).await;
        assert!(!plans.is_empty(), "{name}: no statement read item");
        let sorted = name.starts_with("sort") || name.starts_with("date range");
        let mut checked = 0;
        for (sql, plan) in plans {
            // The statement only an index answers well: for a sort, the page
            // window ordered by the field (without one, every item is sorted);
            // for a filter, the count, which reads every match (the window
            // of ten may rightly walk the newest items instead, when matches
            // are common); for a facet, every statement.
            let wanted = match &shape {
                Shape::Facet { .. } => true,
                Shape::Gather { .. } if sorted => sql.starts_with("SELECT row_to_json"),
                Shape::Gather { .. } => sql.starts_with("SELECT COUNT"),
            };
            if !wanted {
                continue;
            }
            checked += 1;
            let mut found = Vec::new();
            scans(&plan, &mut found);
            let seq = found
                .iter()
                .any(|(k, r, _)| k == "Seq Scan" && r.as_deref() == Some("item"));
            let field_index = found.iter().any(|(_, _, i)| {
                i.as_deref()
                    .is_some_and(|i| i.starts_with("item_fx_") || i.starts_with("item_fv_"))
            });
            if seq || !field_index {
                failures.push(format!("{name}: {found:?}\n  {sql}"));
            }
        }
        assert!(checked > 0, "{name}: no statement checked");
    }
    assert!(
        failures.is_empty(),
        "shapes not served by a field index:\n{}",
        failures.join("\n")
    );
}

/// A field whose setting is off has no index, and a gather on it still
/// answers, from the same rows an indexed field would give.
#[tokio::test]
async fn an_unmarked_field_has_no_index_and_still_answers() {
    let site = site("field_index_unmarked").await;
    site.build_indexes().await;
    let indexes = site.indexes().await;
    assert!(!indexes.contains("item_fx_fx_rec__note"), "{indexes:?}");
    // Long text cannot be indexed.
    assert!(!indexes.contains("item_fx_fx_rec__body"), "{indexes:?}");
    let shape =
        gather(json!({"filters": [f("fields.note", "equals", json!("n4"))], "sorts": newest()}));
    let (ids, _) = run(&site, &shape).await;
    let expected: Vec<String> = sqlx::query_scalar(
        "SELECT id::text FROM item WHERE type = 'fx_rec' AND fields->>'note' = 'n4' \
         ORDER BY created DESC LIMIT 10",
    )
    .fetch_all(&site.pool)
    .await
    .unwrap();
    assert_eq!(ids, expected);
}

/// Turning the setting on queues and builds the index; turning it off, or
/// deleting the field, drops it.
#[tokio::test]
async fn the_setting_creates_and_drops_the_index() {
    let site = site("field_index_toggle").await;
    site.build_indexes().await;
    assert!(!site.indexes().await.contains("item_fx_fx_rec__note"));
    // A type with marked fields has its own index on `created`.
    assert!(site.indexes().await.contains("item_fc_fx_rec__created"));

    site.set_note_filterable(Some(true)).await;
    site.build_indexes().await;
    assert!(site.indexes().await.contains("item_fx_fx_rec__note"));
    let state: String = sqlx::query_scalar(
        "SELECT state FROM item_field_index WHERE item_type = 'fx_rec' AND field_name = 'note'",
    )
    .fetch_one(&site.pool)
    .await
    .unwrap();
    assert_eq!(state, "ready");
    assert!(
        field_index::missing(&site.pool).await.unwrap().is_empty(),
        "every marked field has its index"
    );

    site.set_note_filterable(Some(false)).await;
    site.build_indexes().await;
    assert!(!site.indexes().await.contains("item_fx_fx_rec__note"));

    // No setting: on when a registered gather uses the field, off otherwise.
    site.set_note_filterable(None).await;
    let used = HashSet::from([(TYPE.to_string(), "note".to_string())]);
    field_index::reconcile_with(&site.pool, &site.types.list(), &used)
        .await
        .unwrap();
    field_index::drain(&site.pool).await.unwrap();
    site.gather.refresh_served().await;
    assert!(site.indexes().await.contains("item_fx_fx_rec__note"));
    field_index::reconcile_with(&site.pool, &site.types.list(), &HashSet::new())
        .await
        .unwrap();
    field_index::drain(&site.pool).await.unwrap();
    site.gather.refresh_served().await;
    assert!(!site.indexes().await.contains("item_fx_fx_rec__note"));

    // Deleting the field drops its index.
    assert!(site.indexes().await.contains("item_fx_fx_rec__city"));
    let mut settings = fields_setting(Some(false));
    settings["fields"]
        .as_array_mut()
        .unwrap()
        .retain(|f| f["field_name"] != "city");
    sqlx::query("UPDATE item_type SET settings = $2 WHERE type = $1")
        .bind(TYPE)
        .bind(settings)
        .execute(&site.pool)
        .await
        .unwrap();
    site.types.reload_from_db().await.unwrap();
    site.build_indexes().await;
    assert!(!site.indexes().await.contains("item_fx_fx_rec__city"));

    // An index that went missing behind the record's back is rebuilt.
    sqlx::query("DROP INDEX item_fx_fx_rec__price")
        .execute(&site.pool)
        .await
        .unwrap();
    assert_eq!(field_index::missing(&site.pool).await.unwrap().len(), 1);
    site.build_indexes().await;
    assert!(site.indexes().await.contains("item_fx_fx_rec__price"));
}

/// Every shape answers with the same rows before the indexes exist and after:
/// wrapped values, missing values, numbers held as strings and translations
/// included.
#[tokio::test]
async fn results_are_the_same_with_and_without_the_indexes() {
    let site = site("field_index_same").await;
    let shapes = shapes(&site.pool).await;
    let mut before = Vec::new();
    for (name, shape) in &shapes {
        let (out, _) = run(&site, shape).await;
        assert!(!out.is_empty(), "{name}: no rows; the shape tests nothing");
        before.push(out);
    }
    site.build_indexes().await;
    for ((name, shape), before) in shapes.iter().zip(before) {
        let (after, _) = run(&site, shape).await;
        assert_eq!(before, after, "{name}");
    }
}

/// The numeric range compares numbers, wrapped or bare, JSON numbers or
/// numeric strings, and skips items without the field; the wrapped city
/// values match too.
#[tokio::test]
async fn number_and_wrapped_values_match_what_is_stored() {
    let site = site("field_index_values").await;
    site.build_indexes().await;
    let def = definition(json!({"filters": [
        f("fields.price", "greater_or_equal", json!(100)),
        f("fields.price", "less_or_equal", json!(110))]}));
    let display = QueryDisplay {
        items_per_page: Some(100),
        ..QueryDisplay::default()
    };
    let result = site
        .gather
        .execute_definition(
            &def,
            &display,
            1,
            HashMap::new(),
            LIVE_STAGE_ID,
            &QueryContext::default(),
        )
        .await
        .unwrap();
    let expected: i64 = sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT count(*) FROM generate_series(1, {ITEMS}) n \
         WHERE n % 11 <> 0 AND n % 1000 BETWEEN 100 AND 110"
    )))
    .fetch_one(&site.pool)
    .await
    .unwrap();
    assert_eq!(result.total, u64::try_from(expected).unwrap());

    let def = definition(json!({"filters": [f("fields.city", "equals", json!("city10"))]}));
    let result = site
        .gather
        .execute_definition(
            &def,
            &display,
            1,
            HashMap::new(),
            LIVE_STAGE_ID,
            &QueryContext::default(),
        )
        .await
        .unwrap();
    // n % 40 = 10; every fifth one wrapped. (No access pass in this
    // harness, so unpublished items count too.)
    let expected: i64 = sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT count(*) FROM generate_series(1, {ITEMS}) n WHERE n % 40 = 10"
    )))
    .fetch_one(&site.pool)
    .await
    .unwrap();
    assert_eq!(result.total, u64::try_from(expected).unwrap());
}

/// A gather request sends a fixed number of statements, whatever it filters
/// and sorts on: the field kinds come from the content type registry's
/// memory, not the database.
#[tokio::test]
async fn a_gather_request_sends_an_exact_number_of_statements() {
    let site = site("field_index_count").await;
    site.build_indexes().await;
    for (name, shape) in shapes(&site.pool).await {
        let (_, statements) = run(&site, &shape).await;
        let expected = match (name, &shape) {
            // The option list, a walk down the field's index (not cached
            // yet), then one question per option.
            (_, Shape::Facet { .. }) => 2,
            // The access pass is not wired in this harness: the statement
            // timeout, the count, the page, the commit, the pages' URL
            // aliases.
            _ => 5,
        };
        assert_eq!(statements.len(), expected, "{name}: {statements:#?}");
    }
}

/// A save after the field is marked updates the copy the indexes read (the
/// trigger), so a gather finds the item by its new value at once.
#[tokio::test]
async fn a_save_keeps_the_copy_current() {
    let site = site("field_index_save").await;
    site.build_indexes().await;
    let id: String = sqlx::query_scalar(
        "UPDATE item SET fields = fields || jsonb_build_object('city', jsonb_build_object('value', 'renamed')) \
         WHERE id = md5('item43')::uuid RETURNING id::text",
    )
    .fetch_one(&site.pool)
    .await
    .unwrap();
    let copy: Option<String> =
        sqlx::query_scalar("SELECT filter_values->>'city' FROM item WHERE id = $1::uuid")
            .bind(&id)
            .fetch_one(&site.pool)
            .await
            .unwrap();
    assert_eq!(copy.as_deref(), Some("renamed"));
    let def = definition(json!({"filters": [f("fields.city", "equals", json!("renamed"))]}));
    let result = site
        .gather
        .execute_definition(
            &def,
            &QueryDisplay::default(),
            1,
            HashMap::new(),
            LIVE_STAGE_ID,
            &QueryContext::default(),
        )
        .await
        .unwrap();
    let ids: Vec<&str> = result
        .items
        .iter()
        .filter_map(|r| r["id"].as_str())
        .collect();
    assert_eq!(ids, vec![id.as_str()]);
    assert!(
        result
            .items
            .iter()
            .all(|r| r.get("filter_values").is_none()),
        "the copy column is not part of a result row"
    );
}

/// Several drains at once, as on several servers or a server and the
/// `field-bench index` command, build every index: the builds take turns on
/// the table instead of deadlocking. Before they did, two builds at once
/// failed with `deadlock detected` (`CREATE STATISTICS` and `CREATE INDEX
/// CONCURRENTLY` both take the table's self-conflicting SHARE UPDATE
/// EXCLUSIVE lock), and ten of the nineteen indexes at a million items were
/// left failed.
#[tokio::test]
async fn drains_at_once_build_every_index() {
    let site = site("field_index_together").await;
    field_index::reconcile_with(&site.pool, &site.types.list(), &HashSet::new())
        .await
        .unwrap();
    let (a, b, c, d) = tokio::join!(
        field_index::drain(&site.pool),
        field_index::drain(&site.pool),
        field_index::drain(&site.pool),
        field_index::drain(&site.pool),
    );
    for done in [a, b, c, d] {
        done.unwrap();
    }
    let states: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT field_name, state, error FROM item_field_index ORDER BY field_name")
            .fetch_all(&site.pool)
            .await
            .unwrap();
    assert!(states.len() > 2, "{states:?}");
    assert!(
        states.iter().all(|(_, state, _)| state == "ready"),
        "{states:?}"
    );
    assert!(field_index::missing(&site.pool).await.unwrap().is_empty());
}

/// After the content type cache lets a type's entry expire, Gather still
/// reads each field by its kind, from the copy: a number range compares
/// numbers and uses the field's index.
///
/// Before, Gather read the field kinds from the cache without loading, so
/// for a gather run after the entry expired every field read from `fields`
/// as text. The benchmark at a million items caught it as plans reading
/// `fields` in the middle of a run (Q06 at 12 s), and a number range
/// compared text: `'250' >= '100'`, `'1000' < '110'`.
#[tokio::test]
async fn an_expired_content_type_entry_still_reads_by_kind() {
    let site = site_with_ttl("field_index_expired", Duration::from_secs(1)).await;
    site.build_indexes().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        site.types.get(TYPE).is_none(),
        "the entry has expired, as the test needs"
    );

    let def = definition(json!({"filters": [
        f("fields.price", "greater_or_equal", json!(100)),
        f("fields.price", "less_or_equal", json!(110))]}));
    let (result, statements) = captured(site.gather.execute_definition(
        &def,
        &QueryDisplay::default(),
        1,
        HashMap::new(),
        LIVE_STAGE_ID,
        &QueryContext::default(),
    ))
    .await;
    let result = result.unwrap();
    let expected: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM item WHERE type = 'fx_rec' \
         AND trovato_field_numeric(fields->'price') BETWEEN 100 AND 110",
    )
    .fetch_one(&site.pool)
    .await
    .unwrap();
    assert_eq!(result.total, u64::try_from(expected).unwrap());
    assert!(
        statements
            .iter()
            .any(|s| s.contains("trovato_field_numeric(item.filter_values->'price')")),
        "read from the copy: {statements:#?}"
    );

    // Reconciling then keeps every index: it reads the marked fields from
    // the database, not from the expired cache, which lists no types.
    let before = site.indexes().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let r = field_index::reconcile(&site.pool, &site.types, &site.gather)
        .await
        .unwrap();
    assert_eq!((r.queued_drops, r.trigger_changed), (0, false), "{r:?}");
    field_index::drain(&site.pool).await.unwrap();
    assert_eq!(site.indexes().await, before);
}
