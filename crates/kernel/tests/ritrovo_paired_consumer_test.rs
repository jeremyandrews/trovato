#![allow(clippy::unwrap_used, clippy::expect_used)]
//! P11d paired-consumer check: the **unmodified** `ritrovo_importer` runs
//! against this kernel's plugin-queue v2.
//!
//! The gate is the *compiled artifact*: the committed
//! `plugins/ritrovo_importer/ritrovo_importer.wasm` is loaded into this kernel
//! and driven through the v2 drain.
//!
//! That artifact is now reproducible, which it was not when this test was
//! written. Ritrovo used to be unbuildable outside the monorepo it was split
//! from (its SDK dependency was a path into a Trovato workspace), so the wasm
//! here was a black box of unrecorded provenance. As of FR-24 the ritrovo repo
//! builds standalone against a git-pinned SDK with no Trovato checkout on disk,
//! and this artifact comes from such a build — ritrovo `288cd52`, SDK pinned at
//! the contract-freeze commit `9791c24`. The build is deterministic: two
//! independent clean checkouts produced this artifact byte for byte
//! (sha256 `cd395205…`). Rebuild it with:
//!
//! ```text
//! git clone git@github.com:jeremyandrews/ritrovo.git && cd ritrovo
//! cargo build --target wasm32-wasip1 --release
//! ```
//!
//! Being built against the SDK as it stood at the freeze, and loaded on a
//! kernel that has moved on since, is the point: this test is the freeze
//! guarantee in executable form. It verifies the compatibility that matters:
//!
//! 1. **ABI unchanged** — the importer instantiates on the new kernel, i.e. all
//!    of its declared host imports (`db`, `http`, `logging`, `queue`) still
//!    resolve. `queue_push` is byte-identical (D-48), so this holds.
//! 2. **Declared concurrency read** — its `tap_queue_info` still returns the
//!    `ritrovo_import` queue at concurrency 4 (which the drain now honors,
//!    clamped to the kernel cap).
//! 3. **Worker semantics preserved** — the v2 drain dispatches the real worker
//!    and, on a returned (business-error) result, deletes the job exactly as v1
//!    did; a returned error is a *success*, not a retry.
//!
//! `ritrovo_state`-based self-scheduling runs in `tap_cron`, whose dispatch path
//! in the cron cycle is unchanged by P11d (the drain runs after it, as before),
//! so it is unaffected.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use sqlx::PgPool;

use trovato_kernel::cron::CronService;
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::tap::{RequestServices, RequestState, TapDispatcher, TapRegistry, UserContext};

const IMPORTER: &str = "ritrovo_importer";

static SERIAL: Mutex<()> = Mutex::new(());
static RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build test runtime")
});
static IMPORTER_DISPATCHER: OnceLock<Arc<TapDispatcher>> = OnceLock::new();

fn serial<F: std::future::Future<Output = ()>>(body: F) {
    let _guard = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    RT.block_on(body);
}

/// Stage the committed importer wasm + a manifest (mirroring the real one) in a
/// temp fixture dir and load it. The manifest declares exactly the importer's
/// real host interfaces, so a missing/changed host import would fail the load.
fn importer_dispatcher() -> Arc<TapDispatcher> {
    IMPORTER_DISPATCHER
        .get_or_init(|| {
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
            let wasm_src = repo_root
                .join("plugins")
                .join(IMPORTER)
                .join(format!("{IMPORTER}.wasm"));
            assert!(
                wasm_src.exists(),
                "prebuilt importer wasm missing at {wasm_src:?}"
            );

            let fixture = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(IMPORTER);
            std::fs::create_dir_all(&fixture).unwrap();
            std::fs::copy(&wasm_src, fixture.join(format!("{IMPORTER}.wasm"))).unwrap();
            std::fs::write(
                fixture.join(format!("{IMPORTER}.info.toml")),
                // Mirrors ritrovo_importer.info.toml (taps + capabilities);
                // migrations omitted (not needed to dispatch these taps).
                r#"name = "ritrovo_importer"
description = "Import tech conferences from confs.tech into Trovato"
version = "1.1.0"
api_version = "0.2"
default_enabled = false
dependencies = []

[taps]
implements = ["tap_cron", "tap_queue_info", "tap_queue_worker"]
weight = 0

[capabilities]
host_interfaces = ["db", "http", "logging", "queue"]
raw_sql = true
"#,
            )
            .unwrap();

            let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
            // A load failure here would itself be the paired-consumer STOP signal:
            // it means a host import the unmodified importer needs no longer resolves.
            runtime
                .load_plugin(&fixture)
                .expect("unmodified ritrovo_importer wasm must load on this kernel (ABI check)");
            let runtime = Arc::new(runtime);
            let registry = Arc::new(TapRegistry::from_plugins(&runtime));
            Arc::new(TapDispatcher::new(runtime, registry))
        })
        .clone()
}

async fn fresh_pool() -> PgPool {
    dotenvy::dotenv().ok();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:5432/trovato".to_string());
    let pool = PgPool::connect(&url).await.expect("connect test DB");
    trovato_kernel::db::run_migrations(&pool)
        .await
        .expect("run migrations");
    pool
}

fn cron_with(pool: PgPool, disp: Arc<TapDispatcher>) -> Arc<CronService> {
    let redis = redis::Client::open("redis://127.0.0.1:6379").expect("redis client");
    let mut cron = CronService::new(redis, pool);
    cron.set_tap_dispatcher(disp);
    Arc::new(cron)
}

fn bg_state(disp: &Arc<TapDispatcher>, pool: PgPool) -> RequestState {
    RequestState::new(
        UserContext::background(),
        RequestServices::for_background(pool, None, None, reqwest::Client::new())
            .with_plugin_runtime(disp.runtime().clone()),
    )
}

/// (1) ABI + (2) declared-concurrency: the unmodified importer loads and its
/// `tap_queue_info` still declares `ritrovo_import` at concurrency 4.
#[test]
fn importer_loads_and_declares_concurrency_4() {
    serial(async {
        let pool = fresh_pool().await;
        let disp = importer_dispatcher();

        // Load already succeeded (asserted in the loader); confirm the queue tap.
        let result = disp
            .dispatch_to_plugin("tap_queue_info", "{}", IMPORTER, bg_state(&disp, pool))
            .await
            .expect("importer implements tap_queue_info");
        let info: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        let queues = info.as_array().expect("tap_queue_info returns an array");
        assert_eq!(queues.len(), 1);
        assert_eq!(queues[0]["name"], "ritrovo_import");
        assert_eq!(queues[0]["concurrency"], 4);
    });
}

/// (3) Worker semantics: the v2 drain dispatches the real importer worker; a
/// returned (business-error) result is a success (job deleted), exactly as v1 —
/// no retry, no loss, no trap.
#[test]
fn importer_worker_runs_through_v2_drain() {
    serial(async {
        let pool = fresh_pool().await;
        let disp = importer_dispatcher();

        // Clean any prior importer rows, then enqueue a job whose payload is
        // missing required fields → the worker returns a business error early
        // (before any DB access), which the drain treats as success.
        sqlx::query("DELETE FROM plugin_queue WHERE plugin_name = $1")
            .bind(IMPORTER)
            .execute(&pool)
            .await
            .unwrap();
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO plugin_queue (plugin_name, queue_name, payload, created_at)
             VALUES ($1, 'ritrovo_import', '{\"topic\":null}'::jsonb, $2)",
        )
        .bind(IMPORTER)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();

        let cron = cron_with(pool.clone(), disp);
        let stats = cron.drain_plugin_queues().await.unwrap();

        assert_eq!(stats.succeeded, 1, "worker returned → job consumed");
        assert_eq!(stats.retried, 0, "a returned result is not retried");
        assert_eq!(stats.dead_lettered, 0);
        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*) FROM plugin_queue WHERE plugin_name = $1")
                .bind(IMPORTER)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            remaining, 0,
            "job deleted after worker returned (v1 parity)"
        );
    });
}
