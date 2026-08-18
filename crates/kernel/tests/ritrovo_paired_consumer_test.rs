#![allow(clippy::unwrap_used, clippy::expect_used)]
//! P11d paired-consumer check: the **unmodified** `ritrovo_importer` runs
//! against this kernel's plugin-queue v2.
//!
//! The gate is the *compiled artifact*: the committed
//! `plugins/ritrovo_importer/ritrovo_importer.wasm` is loaded into this kernel
//! and driven through the v2 drain.
//!
//! ## Provenance of the committed artifact
//!
//! **Reproducible from public sources**, which it was not before. Every fact below
//! is checkable by a stranger holding this repository and a network connection:
//!
//! | | |
//! |---|---|
//! | sha256 | `e3361482bca48cbc99f398fae6ef07af582541c89f28fa4b05ac8f41bf96e37e` |
//! | sources | commit `8e72d37` of <https://github.com/jeremyandrews/ritrovo> |
//! | SDK | this repository at `50c46ee`, which that commit pins by `rev` |
//!
//! ```text
//! git clone https://github.com/jeremyandrews/ritrovo.git && cd ritrovo
//! git checkout 8e72d37
//! cargo build --target wasm32-wasip1 --release
//! shasum -a 256 target/wasm32-wasip1/release/ritrovo_importer.wasm
//! ```
//!
//! That recipe was run from a fresh clone and produced this artifact byte for byte.
//! The sha256 is asserted by a test below, so replacing the artifact without
//! refreshing this header fails the suite.
//!
//! The previous header asserted reproducibility and cited "the contract-freeze commit
//! `9791c24`" as the pinned SDK revision — a commit that exists in no published
//! repository, because Ritrovo pinned its SDK at the unpublished development repo. The
//! version sweep replaced that claim with a statement of the gap; this is the gap
//! closed.
//!
//! ## What this test still guarantees, and what it no longer does
//!
//! It is a **paired-consumer check**: a real external plugin, built from public
//! sources against the published SDK, loads on this kernel and runs through the v2
//! drain. That is what it verifies:
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
//!
//! **What it no longer guarantees**, said plainly because the previous artifact did
//! guarantee it by accident: this artifact is built against the *current* SDK, so it
//! no longer demonstrates that a plugin compiled against an *older* SDK still loads.
//! The old artifact did, and could not be rebuilt or audited by anyone — a freeze
//! guarantee whose subject is a black box is weak evidence. This one becomes the older
//! artifact naturally as the kernel moves on, with provenance a reader can check, which
//! is the better trade. The version-compatibility rule itself (`major ==`, `minor <=`)
//! is covered by `plugin/info_parser.rs`'s own tests.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use sqlx::PgPool;

use trovato_kernel::cron::CronService;
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::tap::{RequestServices, RequestState, TapDispatcher, TapRegistry, UserContext};

const IMPORTER: &str = "ritrovo_importer";

/// sha256 of the committed `ritrovo_importer.wasm`, as recorded in this file's
/// provenance header. The header is the only place the artifact's origin is
/// written down, so an artifact swapped without updating it would leave the
/// header quietly wrong; this constant makes that a test failure instead.
const IMPORTER_WASM_SHA256: &str =
    "e3361482bca48cbc99f398fae6ef07af582541c89f28fa4b05ac8f41bf96e37e";

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
                // Mirrors ritrovo_importer.info.toml in the taps and
                // capabilities the load and dispatch depend on; migrations are
                // omitted (not needed to dispatch these taps) and the two
                // version fields are the fixture's own, declaring the current
                // project version rather than Ritrovo's independent one.
                r#"name = "ritrovo_importer"
description = "Import tech conferences from confs.tech into Trovato"
version = "0.99.0"
api_version = "0.99"
default_enabled = false
dependencies = []

[taps]
implements = ["tap_api", "tap_cron", "tap_queue_info", "tap_queue_worker"]
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
    trovato_test_utils::env::load_dotenv();
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

/// The committed artifact is the one this file's provenance header describes.
///
/// Nothing else in the suite would notice a different `.wasm` here: the other
/// two tests assert behavior the importer has had across several builds, so a
/// replacement artifact could pass them while making the header's sha256 false.
#[test]
fn committed_importer_wasm_matches_the_documented_sha256() {
    use sha2::{Digest, Sha256};

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let wasm = repo_root
        .join("plugins")
        .join(IMPORTER)
        .join(format!("{IMPORTER}.wasm"));
    let bytes = std::fs::read(&wasm).expect("read committed importer wasm");
    let digest = format!("{:x}", Sha256::digest(&bytes));
    assert_eq!(
        digest, IMPORTER_WASM_SHA256,
        "the committed {IMPORTER}.wasm is not the artifact this file's \
         provenance header describes; refresh the header (source commit, SDK \
         revision, sha256) in the same change that replaces the artifact"
    );
}
