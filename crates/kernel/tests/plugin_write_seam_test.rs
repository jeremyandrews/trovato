#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B80.4: the write seam (`docs/design/plugin-query-surface.md` section 6).
//!
//! WAT probes call the real host functions:
//!
//! - `db.update` and `db.delete` with an array `where`;
//! - the new `db.upsert` and `db.insert-many`, its bound and its all-or-nothing
//!   rule;
//! - the new `item-api` translation calls, which are `ItemService` writes:
//!   access is decided, `tap_item_update` fires (seen through the
//!   `test_item_writer` witness's `tiw_log`), and the field tier applies to
//!   what a list returns.
//!
//! Requires Postgres, Redis, the translation plugin's table and the
//! `test_item_writer` fixture `.wasm` (built by CI before the test job).

mod common;

use std::sync::Arc;

use common::{TestApp, run_test};
use serde_json::{Value, json};
use sqlx::PgPool;
use trovato_kernel::host;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, Item};
use trovato_kernel::plugin::limits::ResourceLimits;
use trovato_kernel::plugin::{DbPolicy, PluginInfo, PluginState};
use trovato_kernel::tap::{RequestState, UserContext};
use trovato_sdk::host_errors;
use uuid::Uuid;
use wasmtime::{Engine, Linker, Module, Store};

const WITNESS: &str = "test_item_writer";
const TABLE: &str = "b80w_tally";

// =============================================================================
// App: the shared shape, with the witness loaded
// =============================================================================

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("write seam app init thread panicked")
    })
}

/// An app with the witness plugin loaded, which is then disabled again so no
/// app built later loads it.
async fn build_app() -> TestApp {
    trovato_test_utils::env::load_dotenv();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = PgPool::connect(&database_url).await.expect("connect");
    ensure_tiw_log(&pool).await;
    trovato_kernel::plugin::status::install_plugin(&pool, WITNESS, "1.0.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{WITNESS}': {e:#}"));
    let app = TestApp::with_config(|config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
    })
    .await;
    sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = $1")
        .bind(WITNESS)
        .execute(&pool)
        .await
        .expect("disable the witness");
    pool.close().await;
    app
}

/// The key `plugin_item_api_test` and `stage_workflow_test` take for the same
/// table.
const TIW_LOG_LOCK: i64 = 0x0B68_0000_0002;

async fn ensure_tiw_log(pool: &PgPool) {
    let mut tx = pool.begin().await.expect("begin tiw_log setup");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(TIW_LOG_LOCK)
        .execute(&mut *tx)
        .await
        .expect("take tiw_log lock");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS tiw_log (\
           id BIGSERIAL PRIMARY KEY, tap TEXT NOT NULL, item_id TEXT NOT NULL, \
           title TEXT NOT NULL, note TEXT NOT NULL)",
    )
    .execute(&mut *tx)
    .await
    .expect("create tiw_log");
    tx.commit().await.expect("commit tiw_log setup");
}

// =============================================================================
// Probes
// =============================================================================

fn wat_bytes(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A probe calling `module.name` with each argument as a (ptr, len) pair at
/// 64 KiB strides, and, when `out` is set, an output buffer after them.
/// `wide` selects an `i64` result (the row-count calls).
fn probe_wat(module: &str, name: &str, args: &[&str], out: bool, wide: bool) -> String {
    let mut params = String::new();
    let mut data = String::new();
    let mut call = String::new();
    for (i, arg) in args.iter().enumerate() {
        let at = i * 65536;
        params.push_str(" i32 i32");
        data.push_str(&format!(
            "  (data (i32.const {at}) \"{}\")\n",
            wat_bytes(arg)
        ));
        call.push_str(&format!(" (i32.const {at}) (i32.const {})", arg.len()));
    }
    if out {
        params.push_str(" i32 i32");
        call.push_str(" (i32.const 524288) (i32.const 262144)");
    }
    let result = if wide { "i64" } else { "i32" };
    let body = if wide {
        format!("(i32.wrap_i64 (call $f{call}))")
    } else {
        format!("(call $f{call})")
    };
    format!(
        r#"
(module
  (import "{module}" "{name}" (func $f (param{params}) (result {result})))
  (memory (export "memory") 16)
{data}  (func (export "run") (result i32)
    {body}))
"#
    )
}

/// What a probe returned: a code (bytes written, rows, or an error) and the
/// output buffer when it wrote one.
struct Answer {
    code: i32,
    body: String,
}

impl Answer {
    fn json(&self) -> Value {
        assert!(
            self.code > 0,
            "expected output, got host code {}",
            self.code
        );
        serde_json::from_str(&self.body).expect("the host wrote JSON")
    }
}

fn policy() -> DbPolicy {
    let manifest = format!(
        "name = \"write_probe\"\ndescription = \"probe\"\nversion = \"0.1.0\"\n\
         [capabilities]\nhost_interfaces = [\"db\", \"item-api\"]\ndb_tables = [\"{TABLE}\"]\n"
    );
    let info = PluginInfo::parse_str(&manifest, std::path::Path::new("write_probe.info.toml"))
        .expect("probe manifest parses");
    DbPolicy::derive(&info, std::path::Path::new("."))
}

async fn call(wat: &str, user: UserContext, item_background: bool, out: bool) -> Answer {
    let app = app();
    let engine = Engine::new(&wasmtime::Config::new()).unwrap();
    let mut linker: Linker<PluginState> = Linker::new(&engine);
    host::register_all(&mut linker).expect("register host functions");
    let module = Module::new(&engine, wat).expect("compile probe WAT");
    let request = RequestState::new(user, app.state.tap_services().clone());
    let state = PluginState::with_db_policy(
        request,
        "write_probe".to_string(),
        Arc::new(policy()),
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
    let body = if out && code > 0 {
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        let mut buf = vec![0u8; code as usize];
        memory.read(&store, 524288, &mut buf).unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    } else {
        String::new()
    };
    Answer { code, body }
}

async fn db_count(name: &str, args: &[&str]) -> i32 {
    call(
        &probe_wat("trovato:kernel/db", name, args, false, true),
        UserContext::anonymous(),
        false,
        false,
    )
    .await
    .code
}

async fn db_out(name: &str, args: &[&str]) -> Answer {
    call(
        &probe_wat("trovato:kernel/db", name, args, true, false),
        UserContext::anonymous(),
        false,
        true,
    )
    .await
}

async fn item_api(name: &str, args: &[&str], out: bool, user: UserContext) -> Answer {
    call(
        &probe_wat("trovato:kernel/item-api", name, args, out, false),
        user,
        false,
        out,
    )
    .await
}

// =============================================================================
// Fixture
// =============================================================================

/// Advisory-lock key serializing the creation of [`tally`]'s table.
const TALLY_CREATE_LOCK: i64 = 0x_B80E_0000_0001;

/// This file's own table, with a key column so `upsert` has a conflict target,
/// and a run column so each test reads only its own rows.
///
/// `CREATE TABLE IF NOT EXISTS` is not atomic: two tests creating the table at
/// once both find it missing and the second fails on Postgres's catalog
/// (`pg_type_typname_nsp_index`). So the create runs under an advisory lock,
/// in a transaction that releases it.
async fn tally(app: &TestApp) -> String {
    let mut tx = app.db.begin().await.expect("begin table create");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(TALLY_CREATE_LOCK)
        .execute(&mut *tx)
        .await
        .expect("take table create lock");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS b80w_tally \
         (k text PRIMARY KEY, run text NOT NULL, n integer NOT NULL, note text)",
    )
    .execute(&mut *tx)
    .await
    .expect("create own table");
    tx.commit().await.expect("commit table create");
    let run = Uuid::now_v7().simple().to_string();
    let pool = app.db.clone();
    let r = run.clone();
    common::defer_cleanup(move || async move {
        sqlx::query("DELETE FROM b80w_tally WHERE run = $1")
            .bind(r)
            .execute(&pool)
            .await
            .ok();
    });
    run
}

async fn rows(app: &TestApp, run: &str) -> Vec<(String, i32, Option<String>)> {
    sqlx::query_as("SELECT k, n, note FROM b80w_tally WHERE run = $1 ORDER BY k")
        .bind(run)
        .fetch_all(&app.db)
        .await
        .expect("read own table")
}

async fn seed_rows(app: &TestApp, run: &str, ns: &[i32]) {
    for (i, n) in ns.iter().enumerate() {
        sqlx::query("INSERT INTO b80w_tally (k, run, n) VALUES ($1, $2, $3)")
            .bind(format!("{run}-{i}"))
            .bind(run)
            .bind(n)
            .execute(&app.db)
            .await
            .expect("seed own row");
    }
}

/// A published page, created as an administrator, removed when the test ends.
async fn page(app: &TestApp, status: i16) -> Item {
    let item = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "page".to_string(),
                title: format!("b80w {}", Uuid::now_v7().simple()),
                author_id: Uuid::nil(),
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(json!({"body": {"value": "Hello", "format": "plain_text"}})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("write seam test".to_string()),
            },
            &UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]),
        )
        .await
        .expect("create page");
    let pool = app.db.clone();
    let id = item.id;
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item_translation WHERE item_id = $1")
            .bind(id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM tiw_log WHERE item_id = $1")
            .bind(id.to_string())
            .execute(&pool)
            .await;
        let _ = Item::delete(&pool, id).await;
    });
    item
}

async fn taps_for(app: &TestApp, id: Uuid) -> Vec<String> {
    sqlx::query_scalar("SELECT tap FROM tiw_log WHERE item_id = $1 ORDER BY id")
        .bind(id.to_string())
        .fetch_all(&app.db)
        .await
        .expect("read tiw_log")
}

async fn stored_title(app: &TestApp, id: Uuid, language: &str) -> Option<String> {
    sqlx::query_scalar("SELECT title FROM item_translation WHERE item_id = $1 AND language = $2")
        .bind(id)
        .bind(language)
        .fetch_optional(&app.db)
        .await
        .expect("read item_translation")
}

fn editor() -> UserContext {
    UserContext::authenticated(
        Uuid::now_v7(),
        vec![
            "access content".to_string(),
            "view any content".to_string(),
            "edit any content".to_string(),
        ],
    )
}

fn visitor() -> UserContext {
    let mut user = UserContext::anonymous();
    user.permissions = vec!["access content".to_string()];
    user
}

// =============================================================================
// db: array where
// =============================================================================

/// `update` with an array `where`: a range and an equality, ANDed.
#[test]
fn update_takes_an_array_where() {
    run_test(async {
        let app = app();
        let run = tally(app).await;
        seed_rows(app, &run, &[1, 5, 9]).await;

        let changed = db_count(
            "update",
            &[
                TABLE,
                r#"{"note": "big"}"#,
                &json!([
                    {"field": "run", "op": "=", "value": run},
                    {"field": "n", "op": ">=", "value": 5}
                ])
                .to_string(),
            ],
        )
        .await;
        assert_eq!(changed, 2, "two rows have n >= 5");
        let notes: Vec<Option<String>> = rows(app, &run).await.into_iter().map(|r| r.2).collect();
        assert_eq!(notes, vec![None, Some("big".into()), Some("big".into())]);

        // A map still means equality.
        let changed = db_count(
            "update",
            &[
                TABLE,
                r#"{"note": "one"}"#,
                &json!({"k": format!("{run}-0")}).to_string(),
            ],
        )
        .await;
        assert_eq!(changed, 1);
    });
}

/// `delete` with `in`, `!=` and `is_null`.
#[test]
fn delete_takes_an_array_where() {
    run_test(async {
        let app = app();
        let run = tally(app).await;
        seed_rows(app, &run, &[1, 2, 3, 4]).await;

        let deleted = db_count(
            "delete",
            &[
                TABLE,
                &json!([
                    {"field": "run", "op": "=", "value": run},
                    {"field": "n", "op": "in", "value": [1, 2, 3]},
                    {"field": "n", "op": "!=", "value": 2},
                    {"field": "note", "op": "is_null"}
                ])
                .to_string(),
            ],
        )
        .await;
        assert_eq!(deleted, 2, "n in (1, 3)");
        let left: Vec<i32> = rows(app, &run).await.into_iter().map(|r| r.1).collect();
        assert_eq!(left, vec![2, 4]);
    });
}

/// An empty condition list, an unknown operator and an empty `in` are refused
/// before anything runs: none of them may become "every row".
#[test]
fn an_array_where_that_could_mean_every_row_is_refused() {
    run_test(async {
        let app = app();
        let run = tally(app).await;
        seed_rows(app, &run, &[1, 2]).await;

        for bad in [
            json!([]),
            json!([{"field": "n", "op": "~", "value": 1}]),
            json!([{"field": "n", "op": "in", "value": []}]),
            json!([{"field": "n; drop", "op": "=", "value": 1}]),
        ] {
            let code = db_count("delete", &[TABLE, &bad.to_string()]).await;
            assert!(code < 0, "{bad} must be refused, got {code}");
        }
        assert_eq!(rows(app, &run).await.len(), 2, "nothing was deleted");
    });
}

// =============================================================================
// db: upsert and insert-many
// =============================================================================

/// `upsert` inserts, then on the conflict target updates every other column,
/// and returns the row either way.
#[test]
fn upsert_inserts_then_updates() {
    run_test(async {
        let app = app();
        let run = tally(app).await;
        let key = format!("{run}-u");
        let conflict = r#"{"target": ["k"]}"#;

        let first = db_out(
            "upsert",
            &[
                TABLE,
                &json!({"k": key, "run": run, "n": 1}).to_string(),
                conflict,
            ],
        )
        .await;
        assert_eq!(first.json()[0]["n"], 1, "{}", first.body);

        let second = db_out(
            "upsert",
            &[
                TABLE,
                &json!({"k": key, "run": run, "n": 7, "note": "again"}).to_string(),
                conflict,
            ],
        )
        .await;
        assert_eq!(second.json()[0]["n"], 7, "{}", second.body);
        assert_eq!(
            rows(app, &run).await,
            vec![(key.clone(), 7, Some("again".into()))]
        );

        // An explicit update set leaves the other columns alone.
        db_out(
            "upsert",
            &[
                TABLE,
                &json!({"k": key, "run": run, "n": 9, "note": "ignored"}).to_string(),
                r#"{"target": ["k"], "update": ["n"]}"#,
            ],
        )
        .await;
        assert_eq!(rows(app, &run).await, vec![(key, 9, Some("again".into()))]);
    });
}

/// `insert-many` writes every row in one statement and returns the count.
#[test]
fn insert_many_writes_every_row() {
    run_test(async {
        let app = app();
        let run = tally(app).await;
        let batch: Vec<Value> = (0..3)
            .map(|i| json!({"k": format!("{run}-{i}"), "run": run, "n": i}))
            .collect();
        let count = db_count("insert-many", &[TABLE, &json!(batch).to_string()]).await;
        assert_eq!(count, 3);
        assert_eq!(rows(app, &run).await.len(), 3);
    });
}

/// Over 500 rows is refused whole with its own code; rows that disagree on
/// their columns, or a duplicate key, write nothing.
#[test]
fn insert_many_is_all_or_nothing() {
    run_test(async {
        let app = app();
        let run = tally(app).await;

        let too_many: Vec<Value> = (0..501)
            .map(|i| json!({"k": format!("{run}-{i}"), "run": run, "n": i}))
            .collect();
        let code = db_count("insert-many", &[TABLE, &json!(too_many).to_string()]).await;
        assert_eq!(code, host_errors::ERR_DB_BATCH_TOO_LARGE);

        let mixed = json!([
            {"k": format!("{run}-a"), "run": run, "n": 1},
            {"k": format!("{run}-b"), "run": run}
        ]);
        let code = db_count("insert-many", &[TABLE, &mixed.to_string()]).await;
        assert_eq!(code, host_errors::ERR_PARAM_DESERIALIZE, "mixed columns");

        let duplicate = json!([
            {"k": format!("{run}-c"), "run": run, "n": 1},
            {"k": format!("{run}-c"), "run": run, "n": 2}
        ]);
        let code = db_count("insert-many", &[TABLE, &duplicate.to_string()]).await;
        assert!(code < 0, "a duplicate key fails the statement, got {code}");

        assert!(rows(app, &run).await.is_empty(), "nothing was written");
    });
}

/// The new calls are held to the allowlist like the old ones.
#[test]
fn upsert_and_insert_many_need_the_table_declared() {
    run_test(async {
        let code = db_out(
            "upsert",
            &["item", r#"{"title": "x"}"#, r#"{"target": ["id"]}"#],
        )
        .await
        .code;
        assert_eq!(code, host_errors::ERR_TABLE_NOT_DECLARED);
        let code = db_count("insert-many", &["users", r#"[{"name": "x"}]"#]).await;
        assert_eq!(code, host_errors::ERR_TABLE_NOT_DECLARED);
    });
}

// =============================================================================
// item-api: translations
// =============================================================================

fn translation(item: Uuid, title: &str) -> String {
    json!({
        "item_id": item,
        "language": "it",
        "title": title,
        "fields": {"body": {"value": "Ciao", "format": "plain_text"}}
    })
    .to_string()
}

/// An editor saves a translation through `item-api`: it is written, and it is
/// an item write, so `tap_item_update` fires on the other plugins.
#[test]
fn save_translation_writes_and_fires_the_update_tap() {
    run_test(async {
        let app = app();
        common::ensure_translation_table(app);
        let item = page(app, 1).await;

        let saved = item_api(
            "save-translation",
            &[&translation(item.id, "Pagina")],
            true,
            editor(),
        )
        .await;
        assert_eq!(saved.json()["title"], "Pagina", "{}", saved.body);
        assert_eq!(
            stored_title(app, item.id, "it").await.as_deref(),
            Some("Pagina")
        );
        assert!(
            taps_for(app, item.id).await.iter().any(|t| t == "update"),
            "a translation write fires tap_item_update"
        );
    });
}

/// A visitor may not translate; a language the site does not know, or the
/// item's own, is refused; an item that is not there is not found.
#[test]
fn save_translation_is_refused_where_an_edit_would_be() {
    run_test(async {
        let app = app();
        common::ensure_translation_table(app);
        let item = page(app, 1).await;

        let denied = item_api(
            "save-translation",
            &[&translation(item.id, "No")],
            true,
            visitor(),
        )
        .await;
        assert_eq!(denied.code, host_errors::ERR_ITEM_ACCESS_DENIED);

        for language in ["xx", "en"] {
            let body =
                json!({"item_id": item.id, "language": language, "title": "T", "fields": {}});
            let refused = item_api("save-translation", &[&body.to_string()], true, editor()).await;
            assert_eq!(
                refused.code,
                host_errors::ERR_PARAM_DESERIALIZE,
                "language {language}"
            );
        }

        let missing = item_api(
            "save-translation",
            &[&translation(Uuid::now_v7(), "Nessuno")],
            true,
            editor(),
        )
        .await;
        assert_eq!(missing.code, host_errors::ERR_ITEM_NOT_FOUND);
        assert!(stored_title(app, item.id, "it").await.is_none());
    });
}

/// A background call needs `item_background`, as every `item-api` write does.
#[test]
fn a_background_translation_needs_item_background() {
    run_test(async {
        let app = app();
        common::ensure_translation_table(app);
        let item = page(app, 1).await;
        let wat = probe_wat(
            "trovato:kernel/item-api",
            "save-translation",
            &[&translation(item.id, "Notte")],
            true,
            false,
        );
        let refused = call(&wat, UserContext::background(), false, true).await;
        assert_eq!(refused.code, host_errors::ERR_ITEM_BACKGROUND_DENIED);
        let allowed = call(&wat, UserContext::background(), true, true).await;
        assert!(allowed.code > 0, "with the capability: {}", allowed.code);
    });
}

/// `list-translations` returns rows for the items the user may view only, in
/// one call for many items.
#[test]
fn list_translations_returns_what_the_user_may_view() {
    run_test(async {
        let app = app();
        common::ensure_translation_table(app);
        let public = page(app, 1).await;
        let draft = page(app, 0).await;
        for id in [public.id, draft.id] {
            item_api(
                "save-translation",
                &[&translation(id, "Tradotto")],
                true,
                UserContext::administrator(Uuid::nil(), vec![]),
            )
            .await;
        }
        let ids = json!([public.id, draft.id]).to_string();

        let seen = item_api("list-translations", &[&ids], true, visitor()).await;
        let rows = seen.json();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 1, "{}", seen.body);
        assert_eq!(rows[0]["item_id"], json!(public.id.to_string()));
        assert_eq!(rows[0]["language"], "it");
        assert_eq!(rows[0]["fields"]["body"]["value"], "Ciao");

        let all = item_api("list-translations", &[&ids], true, editor()).await;
        assert_eq!(all.json().as_array().unwrap().len(), 2);

        let too_many = json!((0..101).map(|_| Uuid::now_v7()).collect::<Vec<_>>()).to_string();
        let refused = item_api("list-translations", &[&too_many], true, visitor()).await;
        assert_eq!(refused.code, host_errors::ERR_PARAM_DESERIALIZE);
    });
}

/// `delete-translation` is decided like a save, answers whether a row was
/// there, and fires `tap_item_update` when it removed one.
#[test]
fn delete_translation_is_an_item_write() {
    run_test(async {
        let app = app();
        common::ensure_translation_table(app);
        let item = page(app, 1).await;
        item_api(
            "save-translation",
            &[&translation(item.id, "Pagina")],
            true,
            editor(),
        )
        .await;
        sqlx::query("DELETE FROM tiw_log WHERE item_id = $1")
            .bind(item.id.to_string())
            .execute(&app.db)
            .await
            .unwrap();
        let id = item.id.to_string();

        let denied = item_api("delete-translation", &[&id, "it"], false, visitor()).await;
        assert_eq!(denied.code, host_errors::ERR_ITEM_ACCESS_DENIED);

        let removed = item_api("delete-translation", &[&id, "it"], false, editor()).await;
        assert_eq!(removed.code, 1, "a row was removed");
        assert!(stored_title(app, item.id, "it").await.is_none());
        assert!(taps_for(app, item.id).await.iter().any(|t| t == "update"));

        let again = item_api("delete-translation", &[&id, "it"], false, editor()).await;
        assert_eq!(again.code, 0, "nothing left to remove");
    });
}
