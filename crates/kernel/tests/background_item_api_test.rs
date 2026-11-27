#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `item-api` and `query` from `tap_cron` and `tap_queue_worker`, on a real
//! `AppState`.
//!
//! Every other item-api suite builds its `RequestServices` by hand and binds
//! the item service itself, so none of them could see that the services
//! `CronService` builds for its two background paths never had it bound: each
//! `save-item` and `delete-item` from cron or a queue worker answered
//! `ERR_NO_SERVICES` (-10) and wrote nothing.
//!
//! These build the app the way the server does (`AppState::new`, through
//! `TestApp`), with `plugins/test_item_background` as its only plugin, and run
//! the cron cycle (`CronService::run`) and the queue drain
//! (`CronService::drain_plugin_queues`) themselves. Nothing here touches the
//! item service binding. What is asserted is the `item` table.
//!
//! The opposite case loads a renamed copy of the same module whose manifest
//! drops `item_background`, and expects the capability refusal.
//!
//! Requires Postgres, Redis and the fixture `.wasm` (built by CI before the
//! test job).

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::{TestApp, run_test};
use serde_json::{Value, json};
use sqlx::PgPool;
use trovato_kernel::cron::CronResult;
use trovato_kernel::models::SiteConfig;
use trovato_sdk::host_errors;
use uuid::Uuid;

const FIXTURE: &str = "test_item_background";
/// The same module under another name, without `item_background`.
const NOCAP: &str = "test_item_background_nocap";
/// The same fixture under a third and fourth name, for the `query` tests, so
/// their steps never share a variable with the `item-api` tests running at
/// the same time.
const QUERY: &str = "test_item_background_query";
const QUERY_NOCAP: &str = "test_item_background_query_nocap";

/// Serializes enabling a fixture, building an app and disabling it again, so
/// one test's restore cannot land between another's enable and its build.
const FIXTURE_LOCK: i64 = 0x0040_0000_0001;

/// The pool size each app in this file runs with; see [`app_with`].
const APP_MAX_CONNECTIONS: u32 = 8;

/// Copy the fixture into a fresh directory as plugin `name`, optionally
/// without the `item_background` line, and return the directory.
fn fixture_dir(name: &str, item_background: bool) -> PathBuf {
    let source = common::project_root().join("plugins").join(FIXTURE);
    let parent = std::env::temp_dir().join(format!("trovato-u40-{}", Uuid::now_v7().simple()));
    let target = parent.join(name);
    std::fs::create_dir_all(&target).expect("create the fixture directory");
    std::fs::copy(
        source.join(format!("{FIXTURE}.wasm")),
        target.join(format!("{name}.wasm")),
    )
    .unwrap_or_else(|e| {
        panic!(
            "copy the fixture module: {e}\n\
             build it first: cargo build -p {FIXTURE} --target wasm32-wasip1 --release \
             && cp target/wasm32-wasip1/release/{FIXTURE}.wasm plugins/{FIXTURE}/"
        )
    });
    let manifest = std::fs::read_to_string(source.join(format!("{FIXTURE}.info.toml")))
        .expect("read the fixture manifest")
        .replace(
            &format!("name = \"{FIXTURE}\""),
            &format!("name = \"{name}\""),
        );
    let manifest = if item_background {
        manifest
    } else {
        let edited = manifest.replace("item_background = true\n", "");
        assert_ne!(edited, manifest, "the manifest declares item_background");
        edited
    };
    std::fs::write(target.join(format!("{name}.info.toml")), manifest)
        .expect("write the fixture manifest");
    parent
}

/// A fixture's `plugin_status` row as it was before a test enabled it: every
/// column `install_plugin` and a `tap_install` claim overwrite.
type SavedStatus = Option<(i16, String, i64, bool)>;

async fn save_status(pool: &PgPool, name: &str) -> SavedStatus {
    sqlx::query_as(
        "SELECT status, version, updated_at, tap_install_called \
         FROM plugin_status WHERE name = $1",
    )
    .bind(name)
    .fetch_optional(pool)
    .await
    .expect("read the plugin status")
}

async fn restore_status(pool: &PgPool, name: &str, saved: SavedStatus) {
    match saved {
        Some((status, version, updated_at, tap_install_called)) => {
            sqlx::query(
                "UPDATE plugin_status SET status = $1, version = $2, updated_at = $3, \
                 tap_install_called = $4 WHERE name = $5",
            )
            .bind(status)
            .bind(version)
            .bind(updated_at)
            .bind(tap_install_called)
            .bind(name)
            .execute(pool)
            .await
            .expect("restore the plugin status");
        }
        None => {
            sqlx::query("DELETE FROM plugin_status WHERE name = $1")
                .bind(name)
                .execute(pool)
                .await
                .expect("remove the plugin status");
        }
    }
}

/// A connection holding [`FIXTURE_LOCK`], and the pool it came from.
async fn locked() -> (PgPool, sqlx::pool::PoolConnection<sqlx::Postgres>) {
    trovato_test_utils::env::load_dotenv();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = PgPool::connect(&database_url).await.expect("connect");
    let mut lock = pool.acquire().await.expect("a lock connection");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(FIXTURE_LOCK)
        .execute(&mut *lock)
        .await
        .expect("take the fixture lock");
    (pool, lock)
}

async fn unlock(pool: PgPool, mut lock: sqlx::pool::PoolConnection<sqlx::Postgres>) {
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(FIXTURE_LOCK)
        .execute(&mut *lock)
        .await
        .expect("release the fixture lock");
    drop(lock);
    pool.close().await;
}

/// An app whose only plugin is `name`, built by `AppState::new`.
async fn app_with(name: &'static str, item_background: bool) -> TestApp {
    let (pool, lock) = locked().await;
    let saved = save_status(&pool, name).await;
    trovato_kernel::plugin::status::install_plugin(&pool, name, "1.0.0")
        .await
        .expect("enable the fixture");

    let dir = fixture_dir(name, item_background);
    let plugins = dir.clone();
    // Every test in this file keeps its own app alive at once, in one process
    // under libtest. At the harness default of 25 connections each, six apps
    // ask CI's Postgres (100 clients) for more than it has, and the last app to
    // start times out. Each app here runs one step at a time, so a few
    // connections are plenty.
    let app = TestApp::with_config(move |config| {
        config.plugins_dirs = vec![plugins];
        config.database_max_connections = APP_MAX_CONNECTIONS;
    })
    .await;

    restore_status(&pool, name, saved).await;
    unlock(pool, lock).await;
    std::fs::remove_dir_all(&dir).ok();

    assert!(
        app.state
            .tap_dispatcher()
            .registry()
            .has_tap("tap_queue_worker"),
        "the app loaded {name}"
    );
    app
}

fn variable(plugin: &str, key: &str) -> String {
    format!("plugin.{plugin}.{key}")
}

/// The answer the fixture reported for `nonce`, if it has.
async fn answer_for(pool: &PgPool, plugin: &str, key: &str, nonce: &str) -> Option<Value> {
    let stored = SiteConfig::get(pool, &variable(plugin, key)).await.ok()??;
    let answer: Value = match stored {
        Value::String(text) => serde_json::from_str(&text).ok()?,
        other => other,
    };
    (answer.get("nonce").and_then(Value::as_str) == Some(nonce)).then_some(answer)
}

fn step(op: &str, id: Option<Uuid>, title: &str) -> (String, Value) {
    let nonce = Uuid::now_v7().simple().to_string();
    let mut step = json!({ "nonce": nonce, "op": op, "type": "page", "title": title });
    if let Some(id) = id {
        step["id"] = json!(id.to_string());
    }
    (nonce, step)
}

/// Hand `tap_cron` one step, run the cron cycle until it has answered it, and
/// return the answer.
async fn through_cron(
    app: &TestApp,
    plugin: &str,
    op: &str,
    id: Option<Uuid>,
    title: &str,
) -> Value {
    let (nonce, step) = step(op, id, title);
    SiteConfig::set(&app.db, &variable(plugin, "step"), json!(step.to_string()))
        .await
        .expect("set the step");
    // The cron lock is site-wide, and other suites run cron too: a skipped
    // cycle is another holder, not a failure.
    for _ in 0..240 {
        let ran = !matches!(app.state.cron().run().await, CronResult::Skipped);
        if let Some(answer) = answer_for(&app.db, plugin, "cron_result", &nonce).await {
            return answer;
        }
        if !ran {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    panic!("tap_cron never answered the {op} step");
}

/// Queue one step for `tap_queue_worker`, drain until it has answered it, and
/// return the answer.
async fn through_queue(
    app: &TestApp,
    plugin: &str,
    queue: &str,
    op: &str,
    id: Option<Uuid>,
    title: &str,
) -> Value {
    let (nonce, step) = step(op, id, title);
    sqlx::query(
        "INSERT INTO plugin_queue (plugin_name, queue_name, payload, created_at) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(plugin)
    .bind(queue)
    .bind(&step)
    .bind(chrono::Utc::now().timestamp())
    .execute(&app.db)
    .await
    .expect("queue the step");
    for _ in 0..240 {
        app.state
            .cron()
            .drain_plugin_queues()
            .await
            .expect("drain the plugin queue");
        if let Some(answer) = answer_for(&app.db, plugin, "queue_result", &nonce).await {
            return answer;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("tap_queue_worker never answered the {op} step");
}

fn code(answer: &Value) -> i64 {
    answer["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("an answer with a code: {answer}"))
}

fn created_id(answer: &Value) -> Uuid {
    assert_eq!(code(answer), 0, "save-item create: {answer}");
    answer["id"]
        .as_str()
        .and_then(|id| id.parse().ok())
        .unwrap_or_else(|| panic!("the created item's id: {answer}"))
}

async fn title_of(pool: &PgPool, id: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT title FROM item WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
        .expect("read the item")
}

async fn titled(pool: &PgPool, title: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM item WHERE title = $1")
        .bind(title)
        .fetch_one(pool)
        .await
        .expect("count items")
}

/// Remove what a test wrote, pass or fail: its items, the fixture variables it
/// used and its own queue. Only those: the cron and queue tests share the
/// fixture and run at once, so neither may clear the other's step or answer.
fn clean_up_after(
    pool: &PgPool,
    plugin: &'static str,
    keys: &'static [&'static str],
    queue: Option<&'static str>,
    title_prefix: String,
) {
    let pool = pool.clone();
    common::defer_cleanup(move || async move {
        sqlx::query("DELETE FROM item WHERE title LIKE $1")
            .bind(format!("{title_prefix}%"))
            .execute(&pool)
            .await
            .ok();
        let keys: Vec<String> = keys.iter().map(|key| variable(plugin, key)).collect();
        sqlx::query("DELETE FROM site_config WHERE key = ANY($1)")
            .bind(&keys)
            .execute(&pool)
            .await
            .ok();
        if let Some(queue) = queue {
            sqlx::query("DELETE FROM plugin_queue WHERE plugin_name = $1 AND queue_name = $2")
                .bind(plugin)
                .bind(queue)
                .execute(&pool)
                .await
                .ok();
        }
    });
}

fn title(answer: &Value) -> &str {
    answer["title"].as_str().unwrap_or("")
}

/// Create, load, update, delete and load again one item from `tap_cron`.
#[test]
fn tap_cron_writes_items_with_item_background() {
    run_test(async {
        let app = app_with(FIXTURE, true).await;
        let prefix = format!("u40 cron {}", Uuid::now_v7().simple());
        clean_up_after(
            &app.db,
            FIXTURE,
            &["step", "cron_result"],
            None,
            prefix.clone(),
        );

        let created = through_cron(&app, FIXTURE, "create", None, &prefix).await;
        let id = created_id(&created);
        assert_eq!(
            title_of(&app.db, id).await.as_deref(),
            Some(prefix.as_str())
        );

        let loaded = through_cron(&app, FIXTURE, "get", Some(id), "").await;
        assert_eq!(code(&loaded), 0, "get-item: {loaded}");
        assert_eq!(title(&loaded), prefix, "get-item loads the item: {loaded}");

        let renamed = format!("{prefix} (edited)");
        let updated = through_cron(&app, FIXTURE, "update", Some(id), &renamed).await;
        assert_eq!(code(&updated), 0, "save-item update: {updated}");
        assert_eq!(
            title_of(&app.db, id).await.as_deref(),
            Some(renamed.as_str())
        );

        let deleted = through_cron(&app, FIXTURE, "delete", Some(id), "").await;
        assert_eq!(code(&deleted), 0, "delete-item: {deleted}");
        assert_eq!(title_of(&app.db, id).await, None, "the item is gone");

        let gone = through_cron(&app, FIXTURE, "get", Some(id), "").await;
        assert_eq!(code(&gone), 0, "get-item after delete: {gone}");
        assert_eq!(title(&gone), "", "get-item finds nothing: {gone}");
    });
}

/// Create, load, update, delete and load again one item from
/// `tap_queue_worker`.
#[test]
fn tap_queue_worker_writes_items_with_item_background() {
    const QUEUE: &str = "u40_items";
    run_test(async {
        let app = app_with(FIXTURE, true).await;
        let prefix = format!("u40 queue {}", Uuid::now_v7().simple());
        clean_up_after(
            &app.db,
            FIXTURE,
            &["queue_result"],
            Some(QUEUE),
            prefix.clone(),
        );

        let created = through_queue(&app, FIXTURE, QUEUE, "create", None, &prefix).await;
        let id = created_id(&created);
        assert_eq!(
            title_of(&app.db, id).await.as_deref(),
            Some(prefix.as_str())
        );

        let loaded = through_queue(&app, FIXTURE, QUEUE, "get", Some(id), "").await;
        assert_eq!(code(&loaded), 0, "get-item: {loaded}");
        assert_eq!(title(&loaded), prefix, "get-item loads the item: {loaded}");

        let renamed = format!("{prefix} (edited)");
        let updated = through_queue(&app, FIXTURE, QUEUE, "update", Some(id), &renamed).await;
        assert_eq!(code(&updated), 0, "save-item update: {updated}");
        assert_eq!(
            title_of(&app.db, id).await.as_deref(),
            Some(renamed.as_str())
        );

        let deleted = through_queue(&app, FIXTURE, QUEUE, "delete", Some(id), "").await;
        assert_eq!(code(&deleted), 0, "delete-item: {deleted}");
        assert_eq!(title_of(&app.db, id).await, None, "the item is gone");

        let gone = through_queue(&app, FIXTURE, QUEUE, "get", Some(id), "").await;
        assert_eq!(code(&gone), 0, "get-item after delete: {gone}");
        assert_eq!(title(&gone), "", "get-item finds nothing: {gone}");
    });
}

/// Without `item_background`, both paths are refused with the capability
/// error, not with `ERR_NO_SERVICES`, and nothing is written.
#[test]
fn without_item_background_both_paths_are_refused() {
    const QUEUE: &str = "u40_nocap";
    run_test(async {
        let app = app_with(NOCAP, false).await;
        let prefix = format!("u40 nocap {}", Uuid::now_v7().simple());
        clean_up_after(
            &app.db,
            NOCAP,
            &["step", "cron_result", "queue_result"],
            Some(QUEUE),
            prefix.clone(),
        );
        let denied = i64::from(host_errors::ERR_ITEM_BACKGROUND_DENIED);

        let from_cron = through_cron(&app, NOCAP, "create", None, &prefix).await;
        assert_eq!(code(&from_cron), denied, "tap_cron create: {from_cron}");
        let from_queue = through_queue(&app, NOCAP, QUEUE, "create", None, &prefix).await;
        assert_eq!(
            code(&from_queue),
            denied,
            "tap_queue_worker create: {from_queue}"
        );
        // The capability is decided before the id is looked up, so any id
        // shows the refusal.
        let any = Uuid::now_v7();
        let loaded = through_cron(&app, NOCAP, "get", Some(any), "").await;
        assert_eq!(code(&loaded), denied, "tap_cron get: {loaded}");
        let deleted = through_queue(&app, NOCAP, QUEUE, "delete", Some(any), "").await;
        assert_eq!(code(&deleted), denied, "tap_queue_worker delete: {deleted}");
        assert_eq!(titled(&app.db, &prefix).await, 0, "nothing was written");
    });
}

/// Publish an item, so the background viewer is entitled to see it.
async fn publish(pool: &PgPool, id: Uuid) {
    sqlx::query("UPDATE item SET status = 1 WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .expect("publish the item");
}

/// `query` from `tap_cron` and `tap_queue_worker` runs and returns rows.
///
/// Before B249 the services both paths dispatch with bound the item service
/// and not the gather service, so `query` answered `ERR_NO_SERVICES` (-10)
/// from either one after `item_background` had already let it through.
#[test]
fn tap_cron_query_returns_rows_with_item_background() {
    const QUEUE: &str = "b249_query";
    run_test(async {
        let app = app_with(QUERY, true).await;
        let prefix = format!("b249 query {}", Uuid::now_v7().simple());
        clean_up_after(
            &app.db,
            QUERY,
            &["step", "cron_result", "queue_result"],
            Some(QUEUE),
            prefix.clone(),
        );

        let created = through_cron(&app, QUERY, "create", None, &prefix).await;
        let id = created_id(&created);
        publish(&app.db, id).await;

        let no_services = i64::from(host_errors::ERR_NO_SERVICES);
        let from_cron = through_cron(&app, QUERY, "query", None, &prefix).await;
        assert_ne!(code(&from_cron), no_services, "tap_cron query: {from_cron}");
        assert_eq!(code(&from_cron), 0, "tap_cron query: {from_cron}");
        assert_eq!(from_cron["id"], json!(id.to_string()), "{from_cron}");
        assert_eq!(title(&from_cron), prefix, "{from_cron}");
        assert_eq!(from_cron["total"], json!(1), "{from_cron}");

        let from_queue = through_queue(&app, QUERY, QUEUE, "query", None, &prefix).await;
        assert_eq!(code(&from_queue), 0, "tap_queue_worker query: {from_queue}");
        assert_eq!(from_queue["id"], json!(id.to_string()), "{from_queue}");
        assert_eq!(from_queue["total"], json!(1), "{from_queue}");
    });
}

/// Without `item_background`, `query` over content from either background
/// path is still refused with the background denial, now that gather is
/// bound: the binding widens nothing.
#[test]
fn without_item_background_query_is_still_refused() {
    const QUEUE: &str = "b249_query_nocap";
    run_test(async {
        let app = app_with(QUERY_NOCAP, false).await;
        let prefix = format!("b249 query nocap {}", Uuid::now_v7().simple());
        clean_up_after(
            &app.db,
            QUERY_NOCAP,
            &["step", "cron_result", "queue_result"],
            Some(QUEUE),
            prefix.clone(),
        );
        let denied = i64::from(host_errors::ERR_ITEM_BACKGROUND_DENIED);

        let from_cron = through_cron(&app, QUERY_NOCAP, "query", None, &prefix).await;
        assert_eq!(code(&from_cron), denied, "tap_cron query: {from_cron}");
        let from_queue = through_queue(&app, QUERY_NOCAP, QUEUE, "query", None, &prefix).await;
        assert_eq!(
            code(&from_queue),
            denied,
            "tap_queue_worker query: {from_queue}"
        );
    });
}

/// A startup that succeeds hands the pending `tap_install` task its services
/// only once gather is fully bound, and the task then claims the hook.
///
/// `AppState::new` asserts at the hand-off that gather's item-access seam,
/// record registry and user service are all in place. The install task runs
/// on another worker as soon as it is woken, so a hand-off above any of those
/// bindings would let a `query` from `tap_install` read rows that skipped one,
/// such as rows without `author`. Here a hand-off placed too early panics
/// startup instead of racing it.
#[test]
fn a_successful_startup_hands_tap_install_a_fully_bound_gather() {
    const NAME: &str = "test_item_background_install_ok";
    run_test(async {
        let (pool, lock) = locked().await;
        let saved = save_status(&pool, NAME).await;
        trovato_kernel::plugin::status::install_plugin(&pool, NAME, "1.0.0")
            .await
            .expect("enable the fixture");
        sqlx::query("UPDATE plugin_status SET tap_install_called = FALSE WHERE name = $1")
            .bind(NAME)
            .execute(&pool)
            .await
            .expect("mark tap_install pending");

        let dir = fixture_dir(NAME, true);
        let plugins = dir.clone();
        let app = TestApp::with_config(move |config| {
            config.plugins_dirs = vec![plugins];
            config.database_max_connections = APP_MAX_CONNECTIONS;
        })
        .await;

        let mut called = false;
        for _ in 0..80 {
            called =
                sqlx::query_scalar("SELECT tap_install_called FROM plugin_status WHERE name = $1")
                    .bind(NAME)
                    .fetch_one(&pool)
                    .await
                    .expect("read the claim");
            if called {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        restore_status(&pool, NAME, saved).await;
        unlock(pool, lock).await;
        std::fs::remove_dir_all(&dir).ok();
        drop(app);

        assert!(called, "the install task never claimed tap_install");
    });
}

/// A startup that fails after the `tap_install` task is spawned leaves every
/// pending hook pending.
///
/// The task waits for the item service, which `AppState::new` builds after it
/// spawns the task. When startup fails first, the task must not claim: a
/// claim stands whether or not its dispatch succeeds, so a hook claimed by a
/// server that never started would be skipped by the next one that does.
///
/// A template that does not parse is what fails startup here: it is loaded
/// after the task is spawned and before the item service is built.
#[test]
fn a_failed_startup_leaves_tap_install_pending() {
    const NAME: &str = "test_item_background_install";
    run_test(async {
        let (pool, lock) = locked().await;
        let saved = save_status(&pool, NAME).await;
        trovato_kernel::plugin::status::install_plugin(&pool, NAME, "1.0.0")
            .await
            .expect("enable the fixture");
        sqlx::query("UPDATE plugin_status SET tap_install_called = FALSE WHERE name = $1")
            .bind(NAME)
            .execute(&pool)
            .await
            .expect("mark tap_install pending");

        let plugins = fixture_dir(NAME, true);
        let broken =
            std::env::temp_dir().join(format!("trovato-u40-tpl-{}", Uuid::now_v7().simple()));
        std::fs::create_dir_all(&broken).expect("create the template directory");
        std::fs::write(broken.join("u40-broken.html"), "{% if %}").expect("write the template");

        let mut config = trovato_kernel::config::Config::from_env().expect("load the config");
        config.plugins_dirs = vec![plugins.clone()];
        config.templates_dirs = vec![common::project_root().join("templates"), broken.clone()];
        let started = trovato_kernel::state::AppState::new(&config).await;

        // The task would claim within milliseconds of the sender dropping;
        // give it far longer than that.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let called: bool =
            sqlx::query_scalar("SELECT tap_install_called FROM plugin_status WHERE name = $1")
                .bind(NAME)
                .fetch_one(&pool)
                .await
                .expect("read the claim");

        restore_status(&pool, NAME, saved).await;
        unlock(pool, lock).await;
        std::fs::remove_dir_all(&plugins).ok();
        std::fs::remove_dir_all(&broken).ok();

        assert!(
            started.is_err(),
            "a template that does not parse stops startup"
        );
        assert!(!called, "a failed startup claimed tap_install");
    });
}
