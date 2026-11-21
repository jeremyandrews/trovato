#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The `query` host interface: a plugin runs a Gather definition for the
//! request's viewer, stage and language (`docs/design/plugin-query-surface.md`).
//!
//! Each test drives a WAT module against `host::register_all`, the linker
//! production uses, with the shared app's services, so the definition goes
//! through the real `GatherService` and `ItemService`. What is asserted is the
//! host function's code and the rows it wrote. `query-items` is driven the same
//! way for the stage leak section 5 of the note records.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use std::sync::Arc;

use common::{TestApp, run_test, shared_app};
use serde_json::{Value, json};
use trovato_kernel::host;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateUrlAlias, UrlAlias};
use trovato_kernel::plugin::limits::ResourceLimits;
use trovato_kernel::plugin::{DbPolicy, PluginInfo, PluginState};
use trovato_kernel::tap::{RequestState, UserContext};
use trovato_sdk::host_errors;
use uuid::Uuid;
use wasmtime::{Engine, Linker, Module, Store};

/// This file's own content types, so its rows never meet another file's.
const BLOG: &str = "b80q_blog";
const NEWS: &str = "b80q_news";
const DOCS: &str = "b80q_docs";

/// A table of this file's own, standing in for a plugin-owned table.
const OWN_TABLE: &str = "b80q_votes";

/// The two refusals the seam adds (`docs/design/plugin-query-surface.md`
/// section 8). Written out until the SDK defines them.
const ERR_QUERY_INVALID: i32 = -80;
const ERR_QUERY_BOUND: i32 = -81;

// =============================================================================
// Harness
// =============================================================================

/// Escape a string for a WAT data segment.
fn wat_bytes(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A probe that calls `query.run(definition, options)` into an output buffer.
fn query_probe(definition: &str, options: &str) -> String {
    format!(
        r#"
(module
  (import "trovato:kernel/query" "run"
    (func $run (param i32 i32 i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 16)
  (data (i32.const 0) "{def}")
  (data (i32.const 65536) "{opt}")
  (func (export "run") (result i32)
    (call $run (i32.const 0) (i32.const {def_len})
               (i32.const 65536) (i32.const {opt_len})
               (i32.const 131072) (i32.const 786432))))
"#,
        def = wat_bytes(definition),
        def_len = definition.len(),
        opt = wat_bytes(options),
        opt_len = options.len(),
    )
}

/// A probe that calls `item-api.query-items(query)` into an output buffer.
fn query_items_probe(query: &str) -> String {
    format!(
        r#"
(module
  (import "trovato:kernel/item-api" "query-items"
    (func $q (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 16)
  (data (i32.const 0) "{q}")
  (func (export "run") (result i32)
    (call $q (i32.const 0) (i32.const {len}) (i32.const 131072) (i32.const 786432))))
"#,
        q = wat_bytes(query),
        len = query.len(),
    )
}

/// What a probe returned.
struct Answer {
    code: i32,
    body: String,
}

impl Answer {
    fn json(&self) -> Value {
        assert!(self.code > 0, "expected rows, got host code {}", self.code);
        serde_json::from_str(&self.body).expect("the host wrote JSON")
    }

    /// The `items` of a `query.run` result.
    fn items(&self) -> Vec<Value> {
        self.json()["items"].as_array().cloned().unwrap_or_default()
    }

    fn ids(&self) -> Vec<String> {
        self.items()
            .iter()
            .filter_map(|r| r["id"].as_str().map(str::to_string))
            .collect()
    }
}

/// The policy a manifest declaring `db_tables` yields, derived the way the
/// loader derives it.
fn policy(db_tables: &[&str]) -> DbPolicy {
    let tables = db_tables
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let manifest = format!(
        "name = \"query_probe\"\ndescription = \"probe\"\nversion = \"0.1.0\"\n\
         [capabilities]\nhost_interfaces = [\"query\", \"item-api\"]\ndb_tables = [{tables}]\n"
    );
    let info = PluginInfo::parse_str(&manifest, std::path::Path::new("query_probe.info.toml"))
        .expect("probe manifest parses");
    DbPolicy::derive(&info, std::path::Path::new("."))
}

/// Run a WAT probe as `user`, with the shared app's services.
async fn probe(
    app: &TestApp,
    wat: &str,
    user: UserContext,
    policy: DbPolicy,
    item_background: bool,
) -> Answer {
    let engine = Engine::new(&wasmtime::Config::new()).unwrap();
    let mut linker: Linker<PluginState> = Linker::new(&engine);
    host::register_all(&mut linker).expect("register host functions");
    let module = Module::new(&engine, wat).expect("compile probe WAT");

    let request = RequestState::new(user, app.state.tap_services().clone());
    let state = PluginState::with_db_policy(
        request,
        "query_probe".to_string(),
        Arc::new(policy),
        ResourceLimits::default(),
    )
    .with_item_background(item_background);
    let mut store = Store::new(&engine, state);
    let instance = linker
        .instantiate_async(&mut store, &module)
        .await
        .expect("the probe instantiates against the kernel linker");
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .unwrap();
    let code = run.call_async(&mut store, ()).await.expect("run completes");
    let body = if code > 0 {
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        let mut buf = vec![0u8; code as usize];
        memory.read(&store, 131072, &mut buf).unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    } else {
        String::new()
    };
    Answer { code, body }
}

async fn run_query(app: &TestApp, user: UserContext, definition: Value, options: Value) -> Answer {
    probe(
        app,
        &query_probe(&definition.to_string(), &options.to_string()),
        user,
        policy(&[OWN_TABLE]),
        false,
    )
    .await
}

/// The standard anonymous visitor: the anonymous role holds `access content`.
fn visitor() -> UserContext {
    let mut user = UserContext::anonymous();
    user.permissions = vec!["access content".to_string()];
    user
}

/// An editor who may view unpublished content of any type.
fn editor() -> UserContext {
    UserContext::authenticated(
        Uuid::nil(),
        vec!["access content".to_string(), "view any content".to_string()],
    )
}

async fn ensure_types(app: &TestApp) {
    for machine in [BLOG, NEWS, DOCS] {
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, $1, 'query seam fixture', true, 'Title', 'core', '{}'::jsonb) \
             ON CONFLICT (type) DO NOTHING",
        )
        .bind(machine)
        .execute(&app.db)
        .await
        .expect("seed item type");
    }
}

/// Seed one item; every id it returns is deleted when the test ends.
async fn seed(
    app: &TestApp,
    item_type: &str,
    status: i16,
    created: i64,
    stage: Uuid,
    fields: Value,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO item (id, type, title, status, author_id, created, changed, stage_id, fields) \
         VALUES ($1, $2, $3, $4, $5, $6, $6, $7, $8)",
    )
    .bind(id)
    .bind(item_type)
    .bind(format!("{item_type} {created}"))
    .bind(status)
    .bind(Uuid::nil())
    .bind(created)
    .bind(stage)
    .bind(fields)
    .execute(&app.db)
    .await
    .expect("seed item");
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(format!("/item/{id}"))
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
    });
    id
}

async fn alias(app: &TestApp, id: Uuid, path: &str) {
    UrlAlias::create(
        &app.db,
        CreateUrlAlias {
            source: format!("/item/{id}"),
            alias: path.to_string(),
            language: Some("en".to_string()),
            stage_id: Some(LIVE_STAGE_ID),
        },
    )
    .await
    .expect("create alias");
}

/// A stage of the given visibility, removed when the test ends.
async fn stage(app: &TestApp, visibility: &str) -> Uuid {
    let id = Uuid::now_v7();
    let suffix = &id.simple().to_string()[24..];
    sqlx::query(
        "INSERT INTO category_tag (id, category_id, label, weight, created, changed) \
         VALUES ($1, 'stages', $2, 0, 1, 1)",
    )
    .bind(id)
    .bind(format!("b80q {suffix}"))
    .execute(&app.db)
    .await
    .expect("insert stage tag");
    sqlx::query(
        "INSERT INTO stage_config (tag_id, machine_name, visibility, is_default) \
         VALUES ($1, $2, $3, false)",
    )
    .bind(id)
    .bind(format!("b80q_{suffix}"))
    .bind(visibility)
    .execute(&app.db)
    .await
    .expect("insert stage config");
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        sqlx::query("DELETE FROM item WHERE stage_id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM category_tag WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
    });
    id
}

/// This file's own table, with rows whose `status` is 0: an item predicate
/// applied by mistake would drop them.
async fn own_table(app: &TestApp, item_id: Uuid) {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS b80q_votes \
         (id uuid PRIMARY KEY, item_id uuid NOT NULL, status smallint NOT NULL, score integer NOT NULL)",
    )
    .execute(&app.db)
    .await
    .expect("create own table");
    sqlx::query("INSERT INTO b80q_votes (id, item_id, status, score) VALUES ($1, $2, 0, 5)")
        .bind(Uuid::now_v7())
        .bind(item_id)
        .execute(&app.db)
        .await
        .expect("seed own row");
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        sqlx::query("DELETE FROM b80q_votes WHERE item_id = $1")
            .bind(item_id)
            .execute(&pool)
            .await
            .ok();
    });
}

fn of_type(item_type: &str) -> Value {
    json!({
        "base_table": "item",
        "item_type": item_type,
        "sorts": [{"field": "created", "direction": "desc"}]
    })
}

// =============================================================================
// Policy
// =============================================================================

/// A table outside the plugin's allowlist is refused with the existing
/// `table-not-declared` code, as a base and as a join.
#[test]
fn a_definition_naming_an_undeclared_table_is_refused() {
    run_test(async {
        let app = shared_app().await;
        let base = json!({"base_table": "b80q_not_declared", "stage_aware": false});
        let answer = run_query(app, visitor(), base, json!({})).await;
        assert_eq!(answer.code, host_errors::ERR_TABLE_NOT_DECLARED);

        let join = json!({
            "base_table": "item",
            "relationships": [{"name": "x", "target_table": "b80q_not_declared",
                               "join_type": "left", "local_field": "id", "foreign_field": "item_id"}]
        });
        let answer = run_query(app, visitor(), join, json!({})).await;
        assert_eq!(answer.code, host_errors::ERR_TABLE_NOT_DECLARED);

        // A protected table is refused whatever the manifest says.
        let answer = probe(
            app,
            &query_probe(
                &json!({"base_table": "users", "stage_aware": false}).to_string(),
                "{}",
            ),
            visitor(),
            policy(&["users"]),
            false,
        )
        .await;
        assert!(answer.code < 0, "users is never readable: {}", answer.code);
    });
}

/// A content definition cannot opt out of the stage predicate.
#[test]
fn stage_aware_false_on_content_is_refused() {
    run_test(async {
        let app = shared_app().await;
        let mut definition = of_type(BLOG);
        definition["stage_aware"] = json!(false);
        let answer = run_query(app, visitor(), definition, json!({})).await;
        assert_eq!(answer.code, ERR_QUERY_INVALID);
    });
}

// =============================================================================
// The viewer
// =============================================================================

/// An unpublished item is absent for an anonymous visitor and present for an
/// editor, through the seam.
#[test]
fn the_seam_hides_an_unpublished_item_from_a_visitor_and_not_an_editor() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let draft = seed(app, BLOG, 0, 1_000, LIVE_STAGE_ID, json!({})).await;
        let published = seed(app, BLOG, 1, 1_001, LIVE_STAGE_ID, json!({})).await;

        let seen = run_query(app, visitor(), of_type(BLOG), json!({}))
            .await
            .ids();
        assert!(seen.contains(&published.to_string()), "{seen:?}");
        assert!(!seen.contains(&draft.to_string()), "a visitor saw a draft");

        let seen = run_query(app, editor(), of_type(BLOG), json!({}))
            .await
            .ids();
        assert!(seen.contains(&draft.to_string()), "an editor must see it");
    });
}

/// The same through `query-items`, which B68.2 already decides per row; pinned
/// here beside the seam so the two cannot drift.
#[test]
fn query_items_hides_an_unpublished_item_from_a_visitor_and_not_an_editor() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let draft = seed(app, BLOG, 0, 1_002, LIVE_STAGE_ID, json!({})).await;
        let wat = query_items_probe(&json!({"type": BLOG, "limit": 100}).to_string());

        let answer = probe(app, &wat, visitor(), policy(&[]), false).await;
        assert!(answer.code >= 0);
        assert!(!answer.body.contains(&draft.to_string()));

        let answer = probe(app, &wat, editor(), policy(&[]), false).await;
        assert!(answer.body.contains(&draft.to_string()));
    });
}

/// A published item on a stage that is not Live is absent from a live
/// request, through the seam: an `accessible` stage is reachable by direct URL
/// and not listed, and an `internal` one belongs to editors working on it.
#[test]
fn the_seam_reads_only_the_live_stage_in_a_live_request() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let accessible = stage(app, "accessible").await;
        let internal = stage(app, "internal").await;
        let on_accessible = seed(app, NEWS, 1, 1_003, accessible, json!({})).await;
        let on_internal = seed(app, NEWS, 1, 1_004, internal, json!({})).await;

        for user in [visitor(), editor()] {
            let seen = run_query(app, user, of_type(NEWS), json!({})).await.ids();
            assert!(!seen.contains(&on_accessible.to_string()), "{seen:?}");
            assert!(!seen.contains(&on_internal.to_string()), "{seen:?}");
        }
    });
}

/// `query-items` listed every stage: a visitor in a live request saw a
/// published item on an `accessible` stage, which is "not in listings", and an
/// editor saw an internal stage's item.
#[test]
fn query_items_reads_only_the_live_stage_in_a_live_request() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let accessible = stage(app, "accessible").await;
        let internal = stage(app, "internal").await;
        let on_accessible = seed(app, NEWS, 1, 1_005, accessible, json!({})).await;
        let on_internal = seed(app, NEWS, 1, 1_006, internal, json!({})).await;
        let wat = query_items_probe(&json!({"type": NEWS, "limit": 100}).to_string());

        let answer = probe(app, &wat, visitor(), policy(&[]), false).await;
        assert!(answer.code >= 0);
        assert!(
            !answer.body.contains(&on_accessible.to_string()),
            "a visitor listed an item on an accessible stage"
        );
        let answer = probe(app, &wat, editor(), policy(&[]), false).await;
        assert!(
            !answer.body.contains(&on_internal.to_string()),
            "an editor listed an internal stage's item in a live request"
        );
    });
}

/// A background call with no `item_background` reads no content; a plugin's own
/// table needs no such capability.
#[test]
fn a_background_call_needs_item_background_for_content_only() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let wat = query_probe(&of_type(BLOG).to_string(), "{}");
        let answer = probe(app, &wat, UserContext::background(), policy(&[]), false).await;
        assert_eq!(answer.code, host_errors::ERR_ITEM_BACKGROUND_DENIED);

        let answer = probe(app, &wat, UserContext::background(), policy(&[]), true).await;
        assert!(answer.code > 0, "with item_background: {}", answer.code);

        let item = seed(app, BLOG, 1, 1_007, LIVE_STAGE_ID, json!({})).await;
        own_table(app, item).await;
        let own = json!({"base_table": OWN_TABLE, "stage_aware": false});
        let answer = probe(
            app,
            &query_probe(&own.to_string(), "{}"),
            UserContext::background(),
            policy(&[OWN_TABLE]),
            false,
        )
        .await;
        assert!(
            answer.code > 0,
            "own table in the background: {}",
            answer.code
        );
    });
}

// =============================================================================
// A plugin's own table, and joins between the two kinds
// =============================================================================

/// A plugin's own table carries no viewer semantics: its rows come back as
/// they are, `status = 0` included, to an anonymous visitor.
#[test]
fn a_plugins_own_table_has_no_item_predicate() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let item = seed(app, BLOG, 1, 1_008, LIVE_STAGE_ID, json!({})).await;
        own_table(app, item).await;

        // Stage-aware by default; the seam forces it off for an own table,
        // which has no stage_id.
        let definition = json!({
            "base_table": OWN_TABLE,
            "filters": [{"field": "item_id", "operator": "equals", "value": item.to_string()}]
        });
        let rows = run_query(app, visitor(), definition, json!({}))
            .await
            .items();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["status"], 0);
        assert!(rows[0].get("url").is_none(), "an own row has no url");
    });
}

/// Content is always the base. An item base may join the plugin's own table and
/// keeps its access predicate; an own-table base may not join `item`.
#[test]
fn joins_between_content_and_a_plugins_table() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let published = seed(app, DOCS, 1, 1_009, LIVE_STAGE_ID, json!({})).await;
        let draft = seed(app, DOCS, 0, 1_010, LIVE_STAGE_ID, json!({})).await;
        own_table(app, published).await;
        own_table(app, draft).await;

        let item_base = json!({
            "base_table": "item",
            "item_type": DOCS,
            "relationships": [{"name": "v", "target_table": OWN_TABLE, "join_type": "inner",
                               "local_field": "id", "foreign_field": "item_id"}],
            "fields": [{"field_name": "id"}, {"field_name": "score", "table_alias": "v"}]
        });
        let rows = run_query(app, visitor(), item_base, json!({}))
            .await
            .items();
        let ids: Vec<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
        assert!(ids.contains(&published.to_string().as_str()), "{rows:?}");
        assert!(
            !ids.contains(&draft.to_string().as_str()),
            "the join leaked a draft"
        );
        assert!(rows.iter().all(|r| r["score"] == 5));

        let own_base = json!({
            "base_table": OWN_TABLE,
            "relationships": [{"name": "i", "target_table": "item", "join_type": "inner",
                               "local_field": "item_id", "foreign_field": "id"}]
        });
        let answer = run_query(app, visitor(), own_base, json!({})).await;
        assert_eq!(answer.code, ERR_QUERY_INVALID);
    });
}

// =============================================================================
// Bounds
// =============================================================================

/// Each bound has its own refusal rather than a silent cap.
#[test]
fn each_bound_returns_its_error() {
    run_test(async {
        let app = shared_app().await;
        let bound = ERR_QUERY_BOUND;
        let invalid = ERR_QUERY_INVALID;

        let answer = run_query(app, visitor(), of_type(BLOG), json!({"per_page": 101})).await;
        assert_eq!(answer.code, bound, "per_page");

        let mut filters = of_type(BLOG);
        filters["filters"] = json!(
            (0..33)
                .map(|_| json!({"field": "status", "operator": "equals", "value": 1}))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            run_query(app, visitor(), filters, json!({})).await.code,
            bound,
            "filters"
        );

        let mut sorts = of_type(BLOG);
        sorts["sorts"] = json!(
            (0..9)
                .map(|_| json!({"field": "created", "direction": "asc"}))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            run_query(app, visitor(), sorts, json!({})).await.code,
            bound,
            "sorts"
        );

        let mut joins = of_type(BLOG);
        joins["relationships"] = json!(
            (0..4)
                .map(
                    |n| json!({"name": format!("c{n}"), "target_table": "category_tag",
                                "join_type": "left", "local_field": "id", "foreign_field": "id"})
                )
                .collect::<Vec<_>>()
        );
        assert_eq!(
            run_query(app, visitor(), joins, json!({})).await.code,
            bound,
            "joins"
        );

        let mut includes = of_type(BLOG);
        includes["includes"] = json!({"child": {
            "definition": of_type(NEWS), "parent_field": "id", "child_field": "id"}});
        assert_eq!(
            run_query(app, visitor(), includes, json!({})).await.code,
            invalid,
            "includes"
        );

        let mut semantic = of_type(BLOG);
        semantic["filters"] = json!([{"field": "title", "operator": "semantic_similarity",
                                      "value": "anything"}]);
        assert_eq!(
            run_query(app, visitor(), semantic, json!({})).await.code,
            invalid,
            "semantic"
        );
    });
}

// =============================================================================
// The six statements' reads
// =============================================================================

/// Statement 1: the newest blog and news posts together, ordered by `created`,
/// limited, each with its URL: the alias when there is one, `/item/{id}` when
/// there is not.
#[test]
fn two_types_sorted_by_created_with_a_limit_and_urls() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let base = 2_000_000_000 + i64::from(rand_offset());
        let oldest = seed(app, BLOG, 1, base, LIVE_STAGE_ID, json!({})).await;
        let middle = seed(app, NEWS, 1, base + 1, LIVE_STAGE_ID, json!({})).await;
        let newest = seed(app, BLOG, 1, base + 2, LIVE_STAGE_ID, json!({})).await;
        let draft = seed(app, NEWS, 0, base + 3, LIVE_STAGE_ID, json!({})).await;
        let path = format!("/b80q/{newest}");
        alias(app, newest, &path).await;

        let definition = json!({
            "base_table": "item",
            "filters": [
                {"field": "type", "operator": "in", "value": [BLOG, NEWS]},
                {"field": "created", "operator": "greater_or_equal", "value": base},
                {"field": "status", "operator": "equals", "value": 1}
            ],
            "fields": [{"field_name": "id"}, {"field_name": "title"},
                       {"field_name": "type"}, {"field_name": "created"}],
            "sorts": [{"field": "created", "direction": "desc"}]
        });
        let result = run_query(app, editor(), definition, json!({"per_page": 2})).await;
        let rows = result.items();
        let ids: Vec<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
        assert_eq!(
            ids,
            vec![newest.to_string().as_str(), middle.to_string().as_str()],
            "newest first across both types, the draft filtered, limited to two"
        );
        assert_eq!(rows[0]["url"], json!(path));
        assert_eq!(rows[1]["url"], json!(format!("/item/{middle}")));
        assert_eq!(rows[1]["type"], json!(NEWS));
        assert!(
            result.json()["has_next"].as_bool().unwrap(),
            "the oldest is on page 2"
        );
        let _ = (oldest, draft);
    });
}

/// Statement 4: one field of one item, found by the alias it is served at. An
/// unpublished document under an alias reads as nothing to a visitor.
#[test]
fn a_lookup_by_alias_returns_one_field_of_one_item() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let doc = seed(
            app,
            DOCS,
            1,
            1_011,
            LIVE_STAGE_ID,
            json!({"markdown": "# Hello"}),
        )
        .await;
        let draft = seed(
            app,
            DOCS,
            0,
            1_012,
            LIVE_STAGE_ID,
            json!({"markdown": "# Draft"}),
        )
        .await;
        let path = format!("/learn/b80q-{doc}");
        let draft_path = format!("/learn/b80q-{draft}");
        alias(app, doc, &path).await;
        alias(app, draft, &draft_path).await;

        let lookup = |p: &str| {
            json!({
                "base_table": "item",
                "item_type": DOCS,
                "filters": [{"field": "url", "operator": "equals", "value": p}],
                "fields": [{"field_name": "fields.markdown", "label": "markdown"}]
            })
        };
        let rows = run_query(app, visitor(), lookup(&path), json!({"per_page": 1}))
            .await
            .items();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["markdown"], json!("# Hello"));

        let rows = run_query(app, visitor(), lookup(&draft_path), json!({}))
            .await
            .items();
        assert!(rows.is_empty(), "a draft by alias: {rows:?}");

        let rows = run_query(
            app,
            visitor(),
            lookup("/learn/b80q-no-such-page"),
            json!({}),
        )
        .await
        .items();
        assert!(
            rows.is_empty(),
            "an alias that names nothing matches nothing"
        );
    });
}

/// A per-test offset, so two runs against one database do not share `created`
/// values.
fn rand_offset() -> u32 {
    (Uuid::now_v7().as_u128() % 1_000_000) as u32 * 10
}
