#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `item-api` from `tap_cron` and `tap_queue_worker`, on a real `AppState`.
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

/// Serializes enabling a fixture, building an app and disabling it again, so
/// one test's restore cannot land between another's enable and its build.
const FIXTURE_LOCK: i64 = 0x0040_0000_0001;

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

/// An app whose only plugin is `name`, built by `AppState::new`.
async fn app_with(name: &'static str, item_background: bool) -> TestApp {
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

    let previous: Option<i16> =
        sqlx::query_scalar("SELECT status FROM plugin_status WHERE name = $1")
            .bind(name)
            .fetch_optional(&pool)
            .await
            .expect("read the plugin status");
    trovato_kernel::plugin::status::install_plugin(&pool, name, "1.0.0")
        .await
        .expect("enable the fixture");

    let dir = fixture_dir(name, item_background);
    let plugins = dir.clone();
    let app = TestApp::with_config(move |config| config.plugins_dirs = vec![plugins]).await;

    match previous {
        Some(status) => {
            sqlx::query("UPDATE plugin_status SET status = $1 WHERE name = $2")
                .bind(status)
                .bind(name)
                .execute(&pool)
                .await
                .expect("restore the plugin status");
        }
        None => {
            sqlx::query("DELETE FROM plugin_status WHERE name = $1")
                .bind(name)
                .execute(&pool)
                .await
                .expect("remove the plugin status");
        }
    }
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(FIXTURE_LOCK)
        .execute(&mut *lock)
        .await
        .expect("release the fixture lock");
    drop(lock);
    pool.close().await;
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
    op: &str,
    id: Option<Uuid>,
    title: &str,
) -> Value {
    let (nonce, step) = step(op, id, title);
    sqlx::query(
        "INSERT INTO plugin_queue (plugin_name, queue_name, payload, created_at) \
         VALUES ($1, 'u40_items', $2, $3)",
    )
    .bind(plugin)
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

/// Remove what a test wrote, pass or fail.
fn clean_up_after(pool: &PgPool, plugin: &'static str, title_prefix: String) {
    let pool = pool.clone();
    common::defer_cleanup(move || async move {
        sqlx::query("DELETE FROM item WHERE title LIKE $1")
            .bind(format!("{title_prefix}%"))
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM site_config WHERE key LIKE $1")
            .bind(format!("plugin.{plugin}.%"))
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM plugin_queue WHERE plugin_name = $1")
            .bind(plugin)
            .execute(&pool)
            .await
            .ok();
    });
}

/// Create, update and delete one item from `tap_cron`.
#[test]
fn tap_cron_writes_items_with_item_background() {
    run_test(async {
        let app = app_with(FIXTURE, true).await;
        let prefix = format!("u40 cron {}", Uuid::now_v7().simple());
        clean_up_after(&app.db, FIXTURE, prefix.clone());

        let created = through_cron(&app, FIXTURE, "create", None, &prefix).await;
        let id = created_id(&created);
        assert_eq!(
            title_of(&app.db, id).await.as_deref(),
            Some(prefix.as_str())
        );

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
    });
}

/// Create, update and delete one item from `tap_queue_worker`.
#[test]
fn tap_queue_worker_writes_items_with_item_background() {
    run_test(async {
        let app = app_with(FIXTURE, true).await;
        let prefix = format!("u40 queue {}", Uuid::now_v7().simple());
        clean_up_after(&app.db, FIXTURE, prefix.clone());

        let created = through_queue(&app, FIXTURE, "create", None, &prefix).await;
        let id = created_id(&created);
        assert_eq!(
            title_of(&app.db, id).await.as_deref(),
            Some(prefix.as_str())
        );

        let renamed = format!("{prefix} (edited)");
        let updated = through_queue(&app, FIXTURE, "update", Some(id), &renamed).await;
        assert_eq!(code(&updated), 0, "save-item update: {updated}");
        assert_eq!(
            title_of(&app.db, id).await.as_deref(),
            Some(renamed.as_str())
        );

        let deleted = through_queue(&app, FIXTURE, "delete", Some(id), "").await;
        assert_eq!(code(&deleted), 0, "delete-item: {deleted}");
        assert_eq!(title_of(&app.db, id).await, None, "the item is gone");
    });
}

/// Without `item_background`, both paths are refused with the capability
/// error, not with `ERR_NO_SERVICES`, and nothing is written.
#[test]
fn without_item_background_both_paths_are_refused() {
    run_test(async {
        let app = app_with(NOCAP, false).await;
        let prefix = format!("u40 nocap {}", Uuid::now_v7().simple());
        clean_up_after(&app.db, NOCAP, prefix.clone());

        let from_cron = through_cron(&app, NOCAP, "create", None, &prefix).await;
        assert_eq!(
            code(&from_cron),
            i64::from(host_errors::ERR_ITEM_BACKGROUND_DENIED),
            "tap_cron create: {from_cron}"
        );
        let from_queue = through_queue(&app, NOCAP, "create", None, &prefix).await;
        assert_eq!(
            code(&from_queue),
            i64::from(host_errors::ERR_ITEM_BACKGROUND_DENIED),
            "tap_queue_worker create: {from_queue}"
        );
        // The capability is decided before the id is looked up, so any id
        // shows the refusal.
        let existing = Uuid::now_v7();
        let deleted = through_queue(&app, NOCAP, "delete", Some(existing), "").await;
        assert_eq!(
            code(&deleted),
            i64::from(host_errors::ERR_ITEM_BACKGROUND_DENIED),
            "tap_queue_worker delete: {deleted}"
        );
        assert_eq!(titled(&app.db, &prefix).await, 0, "nothing was written");
    });
}
