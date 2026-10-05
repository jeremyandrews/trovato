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
