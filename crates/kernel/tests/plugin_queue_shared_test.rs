#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B68 part 4: **shared plugin queues** (`push-to`).
//!
//! A queue belongs to the plugin that declares it in `tap_queue_info`. The owner
//! may list other plugins in that queue's entry under `shared_with`; a listed
//! plugin pushes with `push-to`, the job runs in the owner's worker at the
//! owner's width, and the worker is told who sent it. Every other push is
//! refused.
//!
//! Two fixtures, driven through the real [`TapDispatcher`] and the real
//! [`CronService`] drain:
//!
//! - `test_queue_owner` declares `shared` (width 2, shared with
//!   `test_queue_sender`), `private`, and `shared_elsewhere` (shared with some
//!   other plugin), and logs every job its worker receives to `tqo_log`.
//! - `test_queue_sender` pushes from `tap_cron` and returns the codes.
//!
//! Build both first:
//! `cargo build -p test_queue_owner -p test_queue_sender --target wasm32-wasip1 --release`
//! and copy each `.wasm` into its `plugins/<name>/` directory.
//!
//! The tests share the owner's queue and `tqo_log`, so they are serialized on
//! [`SERIAL`] and run on one shared runtime ([`RT`]), as the queue v2 suite is.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use serde_json::{Value, json};
use sqlx::PgPool;

use trovato_kernel::cron::CronService;
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::tap::{RequestServices, RequestState, TapDispatcher, TapRegistry, UserContext};

const OWNER: &str = "test_queue_owner";
const SENDER: &str = "test_queue_sender";

/// What `push-to` answers when the owner does not share the queue with the
/// caller (`ERR_QUEUE_PUSH_REFUSED`).
const REFUSED: i64 = -64;

/// Advisory lock key serializing creation of `tqo_log` across test binaries.
const TQO_LOG_LOCK: i64 = 0x0B68_0000_0004;

static SERIAL: Mutex<()> = Mutex::new(());

static RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build test runtime")
});

fn serial<F: std::future::Future<Output = ()>>(body: F) {
    let _guard = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    RT.block_on(body);
}

fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

/// One dispatcher with both fixtures loaded, built once per test binary.
fn dispatcher() -> Arc<TapDispatcher> {
    static DISPATCHER: OnceLock<Arc<TapDispatcher>> = OnceLock::new();
    DISPATCHER
        .get_or_init(|| {
            let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
            for fixture in [OWNER, SENDER] {
                runtime
                    .load_plugin(&plugins_dir().join(fixture))
                    .unwrap_or_else(|e| {
                        panic!(
                            "failed to load fixture '{fixture}': {e:#}\n\
                             build it: cargo build -p {fixture} --target wasm32-wasip1 --release \
                             && cp target/wasm32-wasip1/release/{fixture}.wasm plugins/{fixture}/"
                        )
                    });
            }
            let runtime = Arc::new(runtime);
            let registry = Arc::new(TapRegistry::from_plugins(&runtime));
            Arc::new(TapDispatcher::new(runtime, registry))
        })
        .clone()
}

async fn fresh_pool() -> PgPool {
    trovato_test_utils::env::load_dotenv();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:5432/trovato".to_string());
    let pool = PgPool::connect(&url).await.expect("connect test DB");
    trovato_kernel::db::run_migrations(&pool)
        .await
        .expect("run migrations");
    ensure_tqo_log(&pool).await;
    pool
}

/// Create the owner's log table if it is not there yet.
async fn ensure_tqo_log(pool: &PgPool) {
    let mut tx = pool.begin().await.expect("begin tqo_log setup");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(TQO_LOG_LOCK)
        .execute(&mut *tx)
        .await
        .expect("take tqo_log lock");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS tqo_log (\
           id BIGSERIAL PRIMARY KEY, input TEXT NOT NULL, \
           started TIMESTAMPTZ, finished TIMESTAMPTZ)",
    )
    .execute(&mut *tx)
    .await
    .expect("create tqo_log");
    tx.commit().await.expect("commit tqo_log setup");
}

/// Empty both fixtures' queues and the owner's log.
async fn clean(pool: &PgPool) {
    sqlx::query("DELETE FROM plugin_queue WHERE plugin_name = ANY($1)")
        .bind(vec![OWNER.to_string(), SENDER.to_string()])
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM tqo_log")
        .execute(pool)
        .await
        .unwrap();
}

fn cron_with(pool: PgPool) -> Arc<CronService> {
    let redis = redis::Client::open(trovato_test_utils::env::redis_url()).expect("redis client");
    let mut cron = CronService::new(redis, pool);
    cron.set_tap_dispatcher(dispatcher());
    Arc::new(cron)
}

/// Have the sender push `payload` to `owner`'s `queue` `count` times, from
/// `tap_cron` as production cron runs it; returns the code of each push.
async fn push(pool: &PgPool, owner: &str, queue: &str, payload: Value, count: u64) -> Vec<i64> {
    let disp = dispatcher();
    let state = RequestState::new(
        UserContext::background(),
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
            .with_plugin_runtime(disp.runtime().clone()),
    );
    let input = json!({ "owner": owner, "queue": queue, "payload": payload, "count": count });
    let result = disp
        .dispatch_to_plugin("tap_cron", &input.to_string(), SENDER, state)
        .await
        .expect("the sender's tap_cron ran");
    let output: Value = serde_json::from_str(&result.output).expect("tap_cron returns JSON");
    output["codes"]
        .as_array()
        .unwrap_or_else(|| panic!("no codes in {output}"))
        .iter()
        .map(|c| c.as_i64().unwrap())
        .collect()
}

/// Every row on any queue owned or pushed by the fixtures:
/// `(plugin_name, queue_name, sender)`.
async fn queued(pool: &PgPool) -> Vec<(String, String, Option<String>)> {
    sqlx::query_as(
        "SELECT plugin_name, queue_name, sender FROM plugin_queue \
         WHERE plugin_name = ANY($1) ORDER BY id",
    )
    .bind(vec![OWNER.to_string(), SENDER.to_string()])
    .fetch_all(pool)
    .await
    .expect("read plugin_queue")
}

/// What the owner's worker received, in order.
async fn received(pool: &PgPool) -> Vec<Value> {
    let rows: Vec<String> = sqlx::query_scalar("SELECT input FROM tqo_log ORDER BY id")
        .fetch_all(pool)
        .await
        .expect("read tqo_log");
    rows.iter()
        .map(|r| serde_json::from_str(r).expect("logged input is JSON"))
        .collect()
}

/// A plugin the owner lists pushes, and the job runs in the owner's worker,
/// which is told who sent it.
#[test]
fn a_push_to_a_shared_queue_runs_in_the_owners_worker_with_the_sender_named() {
    serial(async {
        let pool = fresh_pool().await;
        clean(&pool).await;

        let codes = push(&pool, OWNER, "shared", json!({ "job": "hello" }), 1).await;
        assert_eq!(
            codes,
            vec![0],
            "a push to a queue shared with the sender is accepted"
        );

        assert_eq!(
            queued(&pool).await,
            vec![(
                OWNER.to_string(),
                "shared".to_string(),
                Some(SENDER.to_string())
            )],
            "the job is stored under the owner, with the sender recorded by the kernel"
        );

        cron_with(pool.clone()).drain_plugin_queues().await.unwrap();

        assert_eq!(
            received(&pool).await,
            vec![json!({
                "sender": SENDER,
                "queue": "shared",
                "payload": { "job": "hello", "n": 0 }
            })],
            "the owner's worker receives the job, wrapped with who sent it"
        );
        assert!(queued(&pool).await.is_empty(), "the job was drained");

        clean(&pool).await;
    });
}

/// A queue the owner did not share with the sender, a queue it does not
/// declare, and an owner that is not loaded all refuse the push, and nothing is
/// queued.
#[test]
fn a_push_to_a_queue_not_shared_with_the_sender_is_refused() {
    serial(async {
        let pool = fresh_pool().await;
        clean(&pool).await;

        for (owner, queue, why) in [
            (OWNER, "private", "declared, shared with nobody"),
            (OWNER, "shared_elsewhere", "shared with another plugin"),
            (OWNER, "never_declared", "not declared by the owner"),
            ("no_such_plugin", "shared", "no such owner"),
            (SENDER, "shared", "the sender declares no queues"),
        ] {
            let codes = push(&pool, owner, queue, json!({ "job": "nope" }), 1).await;
            assert_eq!(codes, vec![REFUSED], "{owner}/{queue} ({why}) must refuse");
        }

        assert!(
            queued(&pool).await.is_empty(),
            "a refused push queues nothing"
        );

        clean(&pool).await;
    });
}

/// Pushed jobs drain at the width the owner declares for that queue: never
/// more at once, and the width is used.
#[test]
fn pushed_jobs_run_at_the_owners_width_for_that_queue() {
    serial(async {
        let pool = fresh_pool().await;
        clean(&pool).await;
        let cron = cron_with(pool.clone());

        assert_eq!(
            cron.resolved_queue_widths(OWNER)
                .await
                .get("shared")
                .copied(),
            Some(2),
            "the owner declares `shared` at width 2"
        );

        let codes = push(&pool, OWNER, "shared", json!({ "hold_ms": 1500 }), 6).await;
        assert_eq!(codes, vec![0; 6]);

        cron.drain_plugin_queues().await.unwrap();

        let finished: i64 =
            sqlx::query_scalar("SELECT count(*) FROM tqo_log WHERE finished IS NOT NULL")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(finished, 6, "every pushed job ran to completion");

        // For each job, how many jobs were running at the moment it started.
        let widest: i64 = sqlx::query_scalar(
            "SELECT max(running) FROM ( \
               SELECT (SELECT count(*) FROM tqo_log b \
                       WHERE b.started <= a.started AND b.finished > a.started) AS running \
               FROM tqo_log a) per_job",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            widest, 2,
            "jobs pushed to a width-2 queue run two at a time, under the owner's width"
        );

        clean(&pool).await;
    });
}
