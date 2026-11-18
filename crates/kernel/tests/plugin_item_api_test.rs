#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The `item-api` host functions, driven through the real kernel linker.
//!
//! These four functions decided nothing before: they read and wrote through the
//! `Item` model and used the requesting user only as an author id. A plugin
//! handling an anonymous visitor's request could read an unpublished item and
//! its restricted fields, list them, and rewrite or delete any item on the
//! site. `host/item.rs` had one test and it checked that the functions
//! registered, so none of that was visible.
//!
//! Each test here drives a WAT module against `host::register_all` — the same
//! linker production uses — over a live pool, with a request state carrying the
//! user under test. What is asserted is the host function's answer and, for the
//! writes, whether the row actually changed.
//!
//! ## Prerequisites
//!
//! `DATABASE_URL`, migrated, and the field-access reference plugin built:
//! `cargo build -p trovato_field_access_ref --target wasm32-wasip1 --release
//!  && cp target/wasm32-wasip1/release/trovato_field_access_ref.wasm
//!     plugins/trovato_field_access_ref/`

mod common;

use std::sync::Arc;

use serde_json::json;
use sqlx::PgPool;
use trovato_kernel::host;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::plugin::limits::ResourceLimits;
use trovato_kernel::plugin::{DbPolicy, PluginState};
use trovato_kernel::tap::{RequestServices, RequestState, UserContext};
use trovato_sdk::host_errors;
use uuid::Uuid;
use wasmtime::{Engine, Linker, Module, Store};

// =============================================================================
// Harness
// =============================================================================

/// A WAT probe that calls one `item-api` function with a JSON argument.
///
/// All four take a pointer/length pair and three of them take an output buffer
/// too, so one generator covers them with the output pair made optional.
fn item_probe(func: &str, payload: &str, with_out: bool) -> String {
    let escaped = payload.replace('\\', "\\\\").replace('"', "\\\"");
    let len = payload.len();
    let (params, args) = if with_out {
        (
            "i32 i32 i32 i32",
            "\n        (i32.const 8192) (i32.const 16384)",
        )
    } else {
        ("i32 i32", "")
    };
    format!(
        r#"
(module
  (import "trovato:kernel/item-api" "{func}"
    (func $call (param {params}) (result i32)))
  (memory (export "memory") 8)
  (data (i32.const 0) "{escaped}")
  (func (export "run") (result i32)
    (call $call (i32.const 0) (i32.const {len}){args})))
"#
    )
}

/// What a probe returned: the host code, and the JSON it wrote when positive.
struct Answer {
    code: i32,
    body: String,
}

impl Answer {
    fn denied_or_missing(&self) -> bool {
        self.code >= 0 && self.body == "null"
    }
}

async fn probe_pool() -> PgPool {
    trovato_test_utils::env::load_dotenv();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:5432/trovato".to_string());
    let pool = PgPool::connect(&url).await.expect("connect test DB");
    trovato_kernel::db::run_migrations(&pool)
        .await
        .expect("run migrations");
    pool
}

/// Build the plugin state a probe runs in: the user under test, services with
/// the item service wired (so the access decision has something to decide
/// with), and the plugin's declared background capability.
fn probe_state(
    pool: &PgPool,
    user: UserContext,
    item_background: bool,
    items: &Arc<trovato_kernel::content::ItemService>,
) -> PluginState {
    let services =
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new());
    services.set_item_service(items);
    let request = RequestState::new(user, services);
    PluginState::with_db_policy(
        request,
        "item_probe".to_string(),
        Arc::new(DbPolicy::default()),
        ResourceLimits::default(),
    )
    .with_item_background(item_background)
}

/// Run one probe and read back what it wrote.
async fn run(
    pool: &PgPool,
    user: UserContext,
    items: &Arc<trovato_kernel::content::ItemService>,
    func: &str,
    payload: &str,
    with_out: bool,
    item_background: bool,
) -> Answer {
    let engine = Engine::new(&wasmtime::Config::new()).unwrap();
    let mut linker: Linker<PluginState> = Linker::new(&engine);
    host::register_all(&mut linker).expect("register host functions");

    let wat = item_probe(func, payload, with_out);
    let module = Module::new(&engine, &wat).expect("compile item-api probe WAT");
    let mut store = Store::new(&engine, probe_state(pool, user, item_background, items));
    let instance = linker
        .instantiate_async(&mut store, &module)
        .await
        .expect("module instantiates against the kernel linker");
    let run = instance
        .get_typed_func::<(), i32>(&mut store, "run")
        .expect("module exports run");
    let code = run.call_async(&mut store, ()).await.expect("run completes");

    let body = if with_out && code > 0 {
        let memory = instance.get_memory(&mut store, "memory").unwrap();
        let mut buf = vec![0u8; code as usize];
        memory.read(&store, 8192, &mut buf).unwrap();
        String::from_utf8_lossy(&buf).into_owned()
    } else {
        String::new()
    };
    Answer { code, body }
}

fn items_for(pool: &PgPool) -> Arc<trovato_kernel::content::ItemService> {
    Arc::new(common::item_service_with_ref_plugin(pool.clone()))
}

/// Seed one item and return its id. `status` 0 is unpublished, 1 published.
async fn seed_item(
    pool: &PgPool,
    item_type: &str,
    author: Uuid,
    status: i16,
    fields: serde_json::Value,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO item (id, type, title, status, author_id, created, changed, stage_id, fields) \
         VALUES ($1, $2, $3, $4, $5, 1, 1, $6, $7)",
    )
    .bind(id)
    .bind(item_type)
    .bind(format!("probe {id}"))
    .bind(status)
    .bind(author)
    .bind(LIVE_STAGE_ID)
    .bind(fields)
    .execute(pool)
    .await
    .expect("seed item");
    id
}

async fn ensure_type(pool: &PgPool, machine: &str) {
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, $1, 'item-api probe fixture', true, 'Title', 'core', '{}'::jsonb) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(machine)
    .execute(pool)
    .await
    .expect("seed item type");
}

/// A real, rightless user row.
///
/// `item.author_id` is a foreign key, so a `UserContext` carrying an invented
/// uuid cannot write even when the gate lets it through — which would make a
/// "the row is unchanged" assertion pass for the wrong reason, and hide a
/// regression that reopened the write. These users are real, so the only thing
/// standing between them and the row is the decision under test.
async fn rightless_user(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, name, pass, mail, status) VALUES ($1, $2, 'x', $3, 1)")
        .bind(id)
        .bind(format!("itemprobe-{}", id.simple()))
        .bind(format!("itemprobe-{}@example.invalid", id.simple()))
        .execute(pool)
        .await
        .expect("seed a rightless user");
    id
}

async fn some_user(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("a user exists after migrations")
}

async fn status_of(pool: &PgPool, id: Uuid) -> Option<i16> {
    sqlx::query_scalar("SELECT status FROM item WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
        .expect("read status")
}

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

// =============================================================================
// Reads
// =============================================================================

/// An anonymous visitor's plugin could read an unpublished item by id.
#[tokio::test]
async fn anonymous_get_item_cannot_read_an_unpublished_item() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    let author = some_user(&pool).await;
    let id = seed_item(&pool, "page", author, 0, json!({})).await;
    let items = items_for(&pool);

    let answer = run(
        &pool,
        UserContext::anonymous(),
        &items,
        "get-item",
        &id.to_string(),
        true,
        false,
    )
    .await;
    assert!(
        answer.denied_or_missing(),
        "an unpublished item must read as missing to an anonymous caller, got: {} / {}",
        answer.code,
        answer.body
    );

    // The same call as an administrator returns it, so the test is about the
    // decision and not about the item being unreachable.
    let seen = run(
        &pool,
        admin(),
        &items,
        "get-item",
        &id.to_string(),
        true,
        false,
    )
    .await;
    assert!(
        seen.body.contains(&id.to_string()),
        "an administrator must still read it: {} / {}",
        seen.code,
        seen.body
    );

    sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .ok();
}

/// And could list it.
#[tokio::test]
async fn anonymous_query_items_omits_an_unpublished_item() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    let author = some_user(&pool).await;
    let id = seed_item(&pool, "page", author, 0, json!({})).await;
    let items = items_for(&pool);

    let query = json!({"type": "page", "status": 0, "limit": 100}).to_string();
    let answer = run(
        &pool,
        UserContext::anonymous(),
        &items,
        "query-items",
        &query,
        true,
        false,
    )
    .await;
    assert!(answer.code >= 0, "the query itself must succeed");
    assert!(
        !answer.body.contains(&id.to_string()),
        "an unpublished item must not appear in an anonymous listing: {}",
        answer.body
    );

    let seen = run(&pool, admin(), &items, "query-items", &query, true, false).await;
    assert!(
        seen.body.contains(&id.to_string()),
        "an administrator must still see it in the listing"
    );

    sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .ok();
}

/// The field tier, not just the item tier. `trovato_field_access_ref` denies
/// `ssn` on a `person` item to a viewer without `view pii`, so a caller who may
/// see the item must still not see that field through `get-item`.
#[tokio::test]
async fn get_item_drops_a_field_the_user_may_not_view() {
    let pool = probe_pool().await;
    ensure_type(&pool, "person").await;
    let author = some_user(&pool).await;
    let id = seed_item(
        &pool,
        "person",
        author,
        1,
        json!({"ssn": "123-45-6789", "nickname": "visible"}),
    )
    .await;
    let items = items_for(&pool);

    // `access content` is what makes a published item viewable; the point of
    // this test is the field tier, so the item tier has to be satisfied first.
    let without_pii =
        UserContext::authenticated(Uuid::now_v7(), vec!["access content".to_string()]);
    let answer = run(
        &pool,
        without_pii,
        &items,
        "get-item",
        &id.to_string(),
        true,
        false,
    )
    .await;
    assert!(answer.code >= 0, "a published item is readable");
    assert!(
        !answer.body.contains("123-45-6789"),
        "ssn must be dropped for a viewer without `view pii`: {}",
        answer.body
    );
    assert!(
        answer.body.contains("visible"),
        "the ungoverned field must survive: {}",
        answer.body
    );

    let with_pii = UserContext::authenticated(
        Uuid::now_v7(),
        vec!["access content".to_string(), "view pii".to_string()],
    );
    let permitted = run(
        &pool,
        with_pii,
        &items,
        "get-item",
        &id.to_string(),
        true,
        false,
    )
    .await;
    assert!(
        permitted.body.contains("123-45-6789"),
        "a viewer holding `view pii` must still see it: {}",
        permitted.body
    );

    sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .ok();
}

// =============================================================================
// Writes
// =============================================================================

/// A plugin could rewrite any item by id on behalf of a user with no rights to it.
#[tokio::test]
async fn a_user_without_edit_rights_cannot_update_another_users_item() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    let author = some_user(&pool).await;
    let id = seed_item(&pool, "page", author, 1, json!({})).await;
    let items = items_for(&pool);

    let stranger = UserContext::authenticated(rightless_user(&pool).await, vec![]);
    let payload = json!({"id": id.to_string(), "title": "rewritten by a stranger"}).to_string();
    let answer = run(&pool, stranger, &items, "save-item", &payload, true, false).await;
    assert_eq!(
        answer.code,
        host_errors::ERR_ITEM_ACCESS_DENIED,
        "an update without edit access must be refused"
    );

    let title: String = sqlx::query_scalar("SELECT title FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(
        title, "rewritten by a stranger",
        "the row must be unchanged"
    );

    // An administrator still writes it.
    let allowed = run(&pool, admin(), &items, "save-item", &payload, true, false).await;
    assert!(allowed.code >= 0, "an administrator must still update it");

    sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .ok();
}

/// And delete it.
#[tokio::test]
async fn a_user_without_delete_rights_cannot_delete_another_users_item() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    let author = some_user(&pool).await;
    let id = seed_item(&pool, "page", author, 1, json!({})).await;
    let items = items_for(&pool);

    let stranger = UserContext::authenticated(rightless_user(&pool).await, vec![]);
    let answer = run(
        &pool,
        stranger,
        &items,
        "delete-item",
        &id.to_string(),
        false,
        false,
    )
    .await;
    assert_eq!(
        answer.code,
        host_errors::ERR_ITEM_ACCESS_DENIED,
        "a delete without delete access must be refused"
    );
    assert!(
        status_of(&pool, id).await.is_some(),
        "the item must still be there"
    );

    let allowed = run(
        &pool,
        admin(),
        &items,
        "delete-item",
        &id.to_string(),
        false,
        false,
    )
    .await;
    assert_eq!(allowed.code, 0, "an administrator must still delete it");
    assert!(
        status_of(&pool, id).await.is_none(),
        "and the row must be gone"
    );
}

/// A create needs the permission the item routes check for the same act.
#[tokio::test]
async fn a_user_without_the_create_permission_cannot_create() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    let items = items_for(&pool);

    let stranger = UserContext::authenticated(rightless_user(&pool).await, vec![]);
    let payload = json!({"type": "page", "title": "made by a stranger"}).to_string();
    let answer = run(&pool, stranger, &items, "save-item", &payload, true, false).await;
    assert_eq!(
        answer.code,
        host_errors::ERR_ITEM_ACCESS_DENIED,
        "a create without `create page content` must be refused"
    );

    let holder = UserContext::authenticated(
        some_user(&pool).await,
        vec!["create page content".to_string()],
    );
    let allowed = run(&pool, holder, &items, "save-item", &payload, true, false).await;
    assert!(
        allowed.code >= 0,
        "a user holding the permission must still create: {} / {}",
        allowed.code,
        allowed.body
    );

    sqlx::query("DELETE FROM item WHERE title = 'made by a stranger'")
        .execute(&pool)
        .await
        .ok();
}

// =============================================================================
// Background contexts
// =============================================================================

/// A background call has no user to act as, so it needs the capability.
#[tokio::test]
async fn a_background_call_without_the_capability_is_refused() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    let author = some_user(&pool).await;
    let id = seed_item(&pool, "page", author, 1, json!({})).await;
    let items = items_for(&pool);

    for (func, payload, with_out) in [
        ("get-item", id.to_string(), true),
        ("query-items", json!({"type": "page"}).to_string(), true),
        (
            "save-item",
            json!({"id": id.to_string(), "title": "from cron"}).to_string(),
            true,
        ),
        ("delete-item", id.to_string(), false),
    ] {
        let answer = run(
            &pool,
            UserContext::background(),
            &items,
            func,
            &payload,
            with_out,
            false,
        )
        .await;
        assert_eq!(
            answer.code,
            host_errors::ERR_ITEM_BACKGROUND_DENIED,
            "{func} from a background context without the capability must be refused"
        );
    }
    assert!(
        status_of(&pool, id).await.is_some(),
        "and nothing must have been written or deleted"
    );

    sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .ok();
}

/// With the capability, a background call behaves as every call did before.
#[tokio::test]
async fn a_background_call_with_the_capability_acts_with_kernel_authority() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    let author = some_user(&pool).await;
    let id = seed_item(&pool, "page", author, 0, json!({})).await;
    let items = items_for(&pool);

    let answer = run(
        &pool,
        UserContext::background(),
        &items,
        "get-item",
        &id.to_string(),
        true,
        true,
    )
    .await;
    assert!(
        answer.body.contains(&id.to_string()),
        "a declared background caller must read even an unpublished item: {} / {}",
        answer.code,
        answer.body
    );

    let saved = run(
        &pool,
        UserContext::background(),
        &items,
        "save-item",
        &json!({"id": id.to_string(), "status": 1}).to_string(),
        true,
        true,
    )
    .await;
    assert!(saved.code >= 0, "and must still write");
    assert_eq!(status_of(&pool, id).await, Some(1), "the write landed");

    sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .ok();
}

// =============================================================================
// Re-entrancy
// =============================================================================

/// The access decision dispatches `tap_item_access`, and a plugin handling that
/// tap can call `get-item`, which asks for another decision. Without a guard
/// that is unbounded recursion; with one it is a denial.
///
/// Driven directly rather than through a fixture plugin: what has to hold is
/// that an item-api call *made inside a decision* is refused, and the marker
/// the host functions read is the thing under test.
#[tokio::test]
async fn an_item_api_call_inside_an_access_decision_fails_closed() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    let author = some_user(&pool).await;
    let id = seed_item(&pool, "page", author, 1, json!({})).await;
    let items = items_for(&pool);
    let pool2 = pool.clone();
    let items2 = Arc::clone(&items);
    let id2 = id;

    // Outside a decision, an administrator reads the item.
    let outside = run(
        &pool,
        admin(),
        &items,
        "get-item",
        &id.to_string(),
        true,
        false,
    )
    .await;
    assert!(
        outside.body.contains(&id.to_string()),
        "the control: the same call succeeds outside a decision"
    );

    // Inside one, the same call is refused without dispatching anything.
    let inside = trovato_kernel::host::item::deciding_access_for_test(async move {
        run(
            &pool2,
            admin(),
            &items2,
            "get-item",
            &id2.to_string(),
            true,
            false,
        )
        .await
    })
    .await;
    assert_eq!(
        inside.code,
        host_errors::ERR_ITEM_ACCESS_DENIED,
        "an item-api call inside an access decision must fail closed"
    );

    sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .ok();
}

// =============================================================================
// Production wiring
// =============================================================================

/// The one piece the probes above cannot cover: that a real `AppState` actually
/// binds the item service into the services every request-scoped dispatch
/// clones.
///
/// Every other test here wires the handle itself, so all of them would still
/// pass if `AppState` never called `set_item_service`. What would happen in
/// production then is that each request-scoped item-api call reports
/// `ERR_NO_SERVICES` — a fail-closed break rather than a silent hole, but a
/// break, and one no other test would catch.
///
/// The handle is also weak, so this asserts it upgrades while the app is alive:
/// a cycle-free wiring that drops its target immediately would be just as
/// broken as no wiring at all.
#[tokio::test]
async fn the_app_binds_an_item_service_into_its_tap_services() {
    let app = common::shared_app().await;
    assert!(
        app.state.tap_services().item_service().is_some(),
        "AppState must bind the item service the item-api host functions decide with"
    );
}

// =============================================================================
// Writes go through `ItemService` (B68 Part 2)
//
// `save-item` and `delete-item` wrote through the `Item` model, so a plugin's
// write fired none of the item write taps, ran neither write gate, and left the
// item cache serving the old row. These drive a write through the probe and
// watch for it from a second plugin, `test_item_writer`, which logs every item
// write tap it receives to `tiw_log`.
// =============================================================================

/// Serializes creating `tiw_log` across tests, binaries and shards.
const TIW_LOG_LOCK: i64 = 0x0B68_0000_0002;

/// The witness plugin's log table. The fixture writes it with `execute-raw`;
/// the suite creates it, so no plugin migration has to run.
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

/// The witness's log for one title, in the order the taps fired.
async fn taps_titled(pool: &PgPool, title: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT tap FROM tiw_log WHERE title = $1 ORDER BY id")
        .bind(title)
        .fetch_all(pool)
        .await
        .expect("read tiw_log")
}

/// The witness's log for one item id, in order, as `tap` or `tap:note`.
async fn taps_for(pool: &PgPool, id: Uuid) -> Vec<String> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT tap, note FROM tiw_log WHERE item_id = $1 ORDER BY id")
            .bind(id.to_string())
            .fetch_all(pool)
            .await
            .expect("read tiw_log");
    rows.into_iter()
        .map(|(tap, note)| {
            if note.is_empty() {
                tap
            } else {
                format!("{tap}:{note}")
            }
        })
        .collect()
}

async fn forget(pool: &PgPool, id: Uuid, title_prefix: &str) {
    sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .ok();
    sqlx::query("DELETE FROM tiw_log WHERE item_id = $1 OR title LIKE $2")
        .bind(id.to_string())
        .bind(format!("{title_prefix}%"))
        .execute(pool)
        .await
        .ok();
}

/// An item service whose dispatcher has `test_item_writer` loaded, with the
/// services wired so the witness's own item-api calls reach the same service.
struct Witnessed {
    items: Arc<trovato_kernel::content::ItemService>,
    dispatcher: Arc<trovato_kernel::tap::TapDispatcher>,
    services: RequestServices,
}

fn witnessed(pool: &PgPool) -> Witnessed {
    use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
    use trovato_kernel::tap::{TapDispatcher, TapRegistry};

    let name = "test_item_writer";
    let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
    runtime
        .load_plugin(&common::project_root().join("plugins").join(name))
        .unwrap_or_else(|e| {
            panic!(
                "failed to load fixture '{name}': {e:#}\n\
                 build it first: cargo build -p {name} --target wasm32-wasip1 --release \
                 && cp target/wasm32-wasip1/release/{name}.wasm plugins/{name}/"
            )
        });
    let runtime = Arc::new(runtime);
    let registry = Arc::new(TapRegistry::from_plugins(&runtime));
    let dispatcher = Arc::new(TapDispatcher::new(Arc::clone(&runtime), registry));
    let services =
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
            .with_plugin_runtime(Arc::clone(&runtime));
    let items = Arc::new(trovato_kernel::content::ItemService::new(
        pool.clone(),
        Arc::clone(&dispatcher),
        services.clone(),
        std::time::Duration::from_secs(60),
        None,
        None,
    ));
    services.set_item_service(&items);
    Witnessed {
        items,
        dispatcher,
        services,
    }
}

fn unique(prefix: &str) -> String {
    format!("{prefix} {}", Uuid::now_v7().simple())
}

/// A plugin's create fires presave, insert and the index tap on another plugin.
#[tokio::test]
async fn save_item_create_fires_the_write_taps_on_another_plugin() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    ensure_tiw_log(&pool).await;
    let w = witnessed(&pool);
    let title = unique("b68 create");

    let payload = json!({"type": "page", "title": title}).to_string();
    let answer = run(&pool, admin(), &w.items, "save-item", &payload, true, false).await;
    assert!(answer.code >= 0, "the create must succeed: {}", answer.code);
    let id: Uuid = serde_json::from_str::<serde_json::Value>(&answer.body).unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    assert_eq!(
        taps_titled(&pool, &title).await,
        vec!["presave", "insert", "update_index"],
        "a plugin's create must fire the same taps a person's does"
    );
    forget(&pool, id, &title).await;
}

/// A plugin's update fires presave, update and the index tap.
#[tokio::test]
async fn save_item_update_fires_the_write_taps_on_another_plugin() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    ensure_tiw_log(&pool).await;
    let author = some_user(&pool).await;
    let id = seed_item(&pool, "page", author, 1, json!({})).await;
    let w = witnessed(&pool);
    let title = unique("b68 update");

    let payload = json!({"id": id.to_string(), "title": title}).to_string();
    let answer = run(&pool, admin(), &w.items, "save-item", &payload, true, false).await;
    assert!(answer.code >= 0, "the update must succeed: {}", answer.code);
    assert_eq!(
        taps_titled(&pool, &title).await,
        vec!["presave", "update", "update_index"],
        "a plugin's update must fire the same taps a person's does"
    );
    forget(&pool, id, &title).await;
}

/// A plugin's delete fires `tap_item_delete`.
#[tokio::test]
async fn delete_item_fires_the_delete_tap_on_another_plugin() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    ensure_tiw_log(&pool).await;
    let author = some_user(&pool).await;
    let id = seed_item(&pool, "page", author, 1, json!({})).await;
    let w = witnessed(&pool);

    let answer = run(
        &pool,
        admin(),
        &w.items,
        "delete-item",
        &id.to_string(),
        false,
        false,
    )
    .await;
    assert_eq!(answer.code, 0, "the delete must succeed");
    assert_eq!(taps_for(&pool, id).await, vec!["delete"]);
    assert!(status_of(&pool, id).await.is_none(), "and the row is gone");
    forget(&pool, id, "-").await;
}

/// Deleting an id that names no item is a miss, not a success (BL-25).
#[tokio::test]
async fn delete_item_on_a_missing_id_reports_a_miss() {
    let pool = probe_pool().await;
    let items = items_for(&pool);
    let answer = run(
        &pool,
        admin(),
        &items,
        "delete-item",
        &Uuid::now_v7().to_string(),
        false,
        false,
    )
    .await;
    assert!(
        answer.code < 0,
        "deleting nothing must not report success, got {}",
        answer.code
    );
}

/// A user who may edit but not publish cannot publish through a plugin: the
/// service's publish gate decides, as it does for the edit form.
#[tokio::test]
async fn save_item_runs_the_publish_gate() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    let author = some_user(&pool).await;
    let id = seed_item(&pool, "page", author, 0, json!({})).await;
    let items = items_for(&pool);

    let editor = UserContext::authenticated(
        rightless_user(&pool).await,
        vec!["edit any content".to_string(), "access content".to_string()],
    );
    let payload = json!({"id": id.to_string(), "status": 1}).to_string();
    let answer = run(&pool, editor, &items, "save-item", &payload, true, false).await;
    assert_eq!(
        answer.code,
        host_errors::ERR_ITEM_ACCESS_DENIED,
        "publishing without `publish content` must be refused"
    );
    assert_eq!(
        status_of(&pool, id).await,
        Some(0),
        "the item stays unpublished"
    );
    forget(&pool, id, "-").await;
}

/// A user who may edit the item but not a governed field cannot write that
/// field through a plugin: the service's field gate decides.
#[tokio::test]
async fn save_item_runs_the_field_gate() {
    let pool = probe_pool().await;
    ensure_type(&pool, "person").await;
    let author = some_user(&pool).await;
    let id = seed_item(&pool, "person", author, 1, json!({"ssn": "111"})).await;
    let items = items_for(&pool);

    let editor = UserContext::authenticated(
        rightless_user(&pool).await,
        vec!["edit any content".to_string(), "access content".to_string()],
    );
    let payload = json!({"id": id.to_string(), "fields": {"ssn": "999"}}).to_string();
    let answer = run(&pool, editor, &items, "save-item", &payload, true, false).await;
    assert_eq!(
        answer.code,
        host_errors::ERR_ITEM_ACCESS_DENIED,
        "writing a field the user may not edit must be refused"
    );
    let ssn: Option<String> = sqlx::query_scalar("SELECT fields->>'ssn' FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(ssn.as_deref(), Some("111"), "the field is unchanged");
    forget(&pool, id, "-").await;
}

/// The defect as Ritrovo measured it: after a plugin updates an item, the item
/// route kept serving the old title for up to `CACHE_TTL_ITEMS`.
#[test]
fn after_a_plugin_update_the_item_route_serves_the_new_title() {
    common::run_test(async {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};

        let app = common::shared_app().await;
        ensure_type(&app.db, "page").await;
        let author = some_user(&app.db).await;
        let id = seed_item(&app.db, "page", author, 1, json!({})).await;
        let get = || async {
            let response = app
                .request(
                    Request::get(format!("/api/item/{id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["title"]
                .as_str()
                .unwrap()
                .to_string()
        };

        // Warm the cache with the old title.
        assert_eq!(get().await, format!("probe {id}"));

        let title = unique("b68 cache");
        let payload = json!({"id": id.to_string(), "title": title}).to_string();
        let answer = run(
            &app.db,
            admin(),
            app.state.items(),
            "save-item",
            &payload,
            true,
            false,
        )
        .await;
        assert!(answer.code >= 0, "the plugin update must succeed");
        assert_eq!(
            get().await,
            title,
            "the item route must serve the plugin's write at once, not after CACHE_TTL_ITEMS"
        );
        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&app.db)
            .await
            .ok();
    });
}

/// A tap that rewrites the item it was told about terminates: the nested write
/// does not fire its taps back into the plugin making it.
#[tokio::test]
async fn a_tap_that_rewrites_the_same_item_terminates() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    ensure_tiw_log(&pool).await;
    let author = some_user(&pool).await;
    let id = seed_item(&pool, "page", author, 1, json!({})).await;
    let w = witnessed(&pool);
    let title = unique("echo");

    let payload = json!({"id": id.to_string(), "title": title}).to_string();
    let answer = run(&pool, admin(), &w.items, "save-item", &payload, true, false).await;
    assert!(answer.code >= 0, "the outer write must succeed");

    assert_eq!(
        taps_for(&pool, id).await,
        vec!["update", "echo:saved", "update_index"],
        "the echo writes once and its own taps are not fired back into it"
    );
    let stored: String = sqlx::query_scalar("SELECT title FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        stored,
        format!("{title}+"),
        "exactly one nested write landed"
    );
    forget(&pool, id, &title).await;
}

/// A background write from a plugin holding `item_background` goes through the
/// service too, so the taps fire.
#[tokio::test]
async fn a_background_write_with_the_capability_fires_the_taps() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    ensure_tiw_log(&pool).await;
    let author = some_user(&pool).await;
    let id = seed_item(&pool, "page", author, 0, json!({})).await;
    let w = witnessed(&pool);
    let title = unique("b68 background");

    let payload = json!({"id": id.to_string(), "title": title, "status": 1}).to_string();
    let answer = run(
        &pool,
        UserContext::background(),
        &w.items,
        "save-item",
        &payload,
        true,
        true,
    )
    .await;
    assert!(answer.code >= 0, "a declared background write succeeds");
    assert_eq!(status_of(&pool, id).await, Some(1));
    assert_eq!(
        taps_titled(&pool, &title).await,
        vec!["presave", "update", "update_index"]
    );
    forget(&pool, id, &title).await;
}

/// `language` on create is stored when the site has it and refused otherwise.
#[tokio::test]
async fn save_item_create_takes_a_known_language_and_refuses_an_unknown_one() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    let items = items_for(&pool);
    let title = unique("b68 language");

    let known = json!({"type": "page", "title": title, "language": "en"}).to_string();
    let answer = run(&pool, admin(), &items, "save-item", &known, true, false).await;
    assert!(answer.code >= 0, "a known language is accepted");
    let body: serde_json::Value = serde_json::from_str(&answer.body).unwrap();
    assert_eq!(body["language"], "en");
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();

    let unknown = json!({"type": "page", "title": title, "language": "xx-none"}).to_string();
    let refused = run(&pool, admin(), &items, "save-item", &unknown, true, false).await;
    assert_eq!(
        refused.code,
        host_errors::ERR_PARAM_DESERIALIZE,
        "a language the site does not have is refused"
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM item WHERE title = $1")
        .bind(&title)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "and nothing was created for it");
    forget(&pool, id, &title).await;
}

/// The item-api bindings a plugin calls, end to end: create, read, list,
/// update, delete, and a second delete that is a miss.
#[tokio::test]
async fn the_item_api_bindings_round_trip_from_a_plugin() {
    let pool = probe_pool().await;
    ensure_type(&pool, "page").await;
    ensure_tiw_log(&pool).await;
    let w = witnessed(&pool);
    let title = unique("b68 roundtrip");

    let state = RequestState::new(admin(), w.services.clone());
    let result = w
        .dispatcher
        .dispatch_to_plugin(
            "tap_queue_worker",
            &json!({"type": "page", "title": title}).to_string(),
            "test_item_writer",
            state,
        )
        .await
        .expect("the fixture's round trip ran");
    let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    let id: Uuid = out["id"].as_str().expect("created").parse().unwrap();

    assert_eq!(out["created_title"], title.as_str());
    assert_eq!(out["get"], title.as_str());
    assert_eq!(out["query_found"], true);
    assert_eq!(out["update"], format!("{title} (edited)"));
    assert_eq!(out["delete"], 0);
    assert!(
        out["delete_again"].as_i64().unwrap() < 0,
        "a second delete is a miss: {out}"
    );
    assert_eq!(out["get_after_delete"], serde_json::Value::Null);
    forget(&pool, id, &title).await;
}
