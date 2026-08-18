#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `trovato_spam` — AI comment moderation, end to end through the real plugin.
//!
//! Drives the real WASM plugin through the real kernel path: `tap_comment_insert`
//! → the queue host function → `plugin_queue` → the v2 drain →
//! `tap_queue_worker` → the `ai-request` host function. No stubs.
//!
//! What can be asserted without a provider is exactly what matters most: the
//! **failure posture**. With no AI provider configured, classification cannot
//! happen, and the plugin must leave the comment where it was rather than guess.
//! These tests prove that, and that the job survives to be retried.
//!
//! Build the plugin first:
//! `cargo build -p trovato_spam --target wasm32-wasip1 --release \
//!   && cp target/wasm32-wasip1/release/trovato_spam.wasm plugins/trovato_spam/`
//!
//! Requires Postgres + Redis; runs in CI.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use sqlx::{PgPool, Row};

use trovato_kernel::cron::CronService;
use trovato_kernel::plugin::{DbPolicy, PluginConfig, PluginInfo, PluginRuntime};
use trovato_kernel::services::ai_provider::AiProviderService;
use trovato_kernel::tap::{RequestServices, RequestState, TapDispatcher, TapRegistry, UserContext};

/// The plugin under test, and its `plugin_queue.plugin_name`.
const PLUGIN: &str = "trovato_spam";

/// The queue it declares.
const QUEUE: &str = "comment_moderation";

/// Stored comment statuses, as the kernel's `CommentStatus` defines them.
const STATUS_PUBLISHED: i16 = 1;
const STATUS_PENDING: i16 = 2;

/// The drain claims by `plugin_name`, so these tests would steal each other's
/// rows. Serialized at the OS-thread level, as the queue-v2 suite is.
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

/// One runtime for the binary: each `PluginRuntime` reserves a large
/// pooling-allocator slab.
static DISPATCHER: OnceLock<Arc<TapDispatcher>> = OnceLock::new();

fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

fn dispatcher() -> Arc<TapDispatcher> {
    DISPATCHER
        .get_or_init(|| {
            let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
            runtime
                .load_plugin(&plugins_dir().join(PLUGIN))
                .unwrap_or_else(|e| {
                    panic!(
                        "failed to load '{PLUGIN}': {e:#}\n\
                         build it: cargo build -p {PLUGIN} --target wasm32-wasip1 --release \
                         && cp target/wasm32-wasip1/release/{PLUGIN}.wasm plugins/{PLUGIN}/"
                    )
                });
            let runtime = Arc::new(runtime);
            let registry = Arc::new(TapRegistry::from_plugins(&runtime));
            Arc::new(TapDispatcher::new(runtime, registry))
        })
        .clone()
}

async fn pool() -> PgPool {
    trovato_test_utils::env::load_dotenv();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:5432/trovato".to_string());
    let pool = PgPool::connect(&url).await.expect("connect test DB");
    trovato_kernel::db::run_migrations(&pool)
        .await
        .expect("run migrations");
    pool
}

/// A background `RequestState` with the services the plugin needs: a real pool
/// (for the queue push and the verdict write) and a present-but-empty AI provider
/// service, so an `ai_request` reaches provider resolution and fails there.
fn background_state(pool: &PgPool, runtime: Arc<PluginRuntime>) -> RequestState {
    let ai_providers = Some(Arc::new(AiProviderService::new(pool.clone())));
    let services =
        RequestServices::for_background(pool.clone(), ai_providers, None, reqwest::Client::new())
            .with_plugin_runtime(runtime);
    RequestState::new(UserContext::background(), services)
}

fn cron_with(pool: PgPool, disp: Arc<TapDispatcher>) -> Arc<CronService> {
    let redis = redis::Client::open("redis://127.0.0.1:6379").expect("redis client");
    let mut cron = CronService::new(redis, pool);
    cron.set_tap_dispatcher(disp);
    Arc::new(cron)
}

async fn clean_queue(pool: &PgPool) {
    sqlx::query("DELETE FROM plugin_queue WHERE plugin_name = $1")
        .bind(PLUGIN)
        .execute(pool)
        .await
        .unwrap();
}

/// Seed an item type, an item and a comment, and return the comment id.
///
/// Written with plain SQL rather than the services, because this binary builds its
/// own runtime and pool rather than using the shared `TestApp`.
async fn seed_comment(pool: &PgPool, status: i16) -> uuid::Uuid {
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let item_type = format!("spam_test_{}", &suffix[..8]);
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'Spam Test', 'fixture', true, 'Title', 'core', '{}'::jsonb) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(&item_type)
    .execute(pool)
    .await
    .expect("seed item type");

    // An author that certainly exists: the first user in the table, else the nil
    // UUID's row created here.
    let author: uuid::Uuid = sqlx::query_scalar("SELECT id FROM users ORDER BY created LIMIT 1")
        .fetch_optional(pool)
        .await
        .expect("look for a user")
        .unwrap_or(uuid::Uuid::nil());

    let item_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO item (id, type, title, status, author_id, created, changed, promote, sticky, fields, stage_id, language) \
         VALUES ($1, $2, 'Spam Test Item', 1, $3, $4, $4, 0, 0, '{}'::jsonb, $5, 'en')",
    )
    .bind(item_id)
    .bind(&item_type)
    .bind(author)
    .bind(now)
    .bind(trovato_kernel::models::stage::LIVE_STAGE_ID)
    .execute(pool)
    .await
    .expect("seed item");

    let comment_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO comment (id, item_id, parent_id, author_id, body, body_format, status, created, changed) \
         VALUES ($1, $2, NULL, $3, 'Buy cheap watches at example.com', 'filtered_html', $4, $5, $5)",
    )
    .bind(comment_id)
    .bind(item_id)
    .bind(author)
    .bind(status)
    .bind(now)
    .execute(pool)
    .await
    .expect("seed comment");

    comment_id
}

async fn comment_status(pool: &PgPool, id: uuid::Uuid) -> i16 {
    sqlx::query_scalar("SELECT status FROM comment WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read comment status")
}

/// The rows this plugin has queued.
async fn queued_jobs(pool: &PgPool) -> Vec<(String, serde_json::Value, i32)> {
    sqlx::query("SELECT queue_name, payload, attempts FROM plugin_queue WHERE plugin_name = $1")
        .bind(PLUGIN)
        .fetch_all(pool)
        .await
        .expect("read queue")
        .into_iter()
        .map(|row| {
            (
                row.get::<String, _>("queue_name"),
                row.get::<serde_json::Value, _>("payload"),
                row.get::<i32, _>("attempts"),
            )
        })
        .collect()
}

/// A new comment is queued for classification, carrying what the classifier needs.
#[test]
fn a_new_comment_is_queued_for_classification() {
    serial(async {
        let pool = pool().await;
        clean_queue(&pool).await;

        let comment_id = seed_comment(&pool, STATUS_PENDING).await;
        let disp = dispatcher();
        let state = background_state(&pool, disp.runtime().clone());

        let comment = serde_json::json!({
            "id": comment_id,
            "item_id": uuid::Uuid::now_v7(),
            "author_id": uuid::Uuid::nil(),
            "body": "Buy cheap watches at example.com",
            "status": STATUS_PENDING,
        });

        let result = disp
            .dispatch_to_plugin("tap_comment_insert", &comment.to_string(), PLUGIN, state)
            .await
            .expect("the plugin implements tap_comment_insert");

        let output: serde_json::Value =
            serde_json::from_str(&result.output).expect("plugin output is JSON");
        assert_eq!(
            output.get("queued").and_then(|v| v.as_bool()),
            Some(true),
            "output was {}",
            result.output
        );

        let jobs = queued_jobs(&pool).await;
        assert_eq!(jobs.len(), 1, "one classification job, got {jobs:?}");
        let (queue_name, payload, _) = &jobs[0];
        assert_eq!(queue_name, QUEUE);
        assert_eq!(
            payload.get("comment_id").and_then(|v| v.as_str()),
            Some(comment_id.to_string().as_str()),
            "the job must name the comment it is about"
        );
        assert!(
            payload.get("body").and_then(|v| v.as_str()).is_some(),
            "the job must carry the comment body for the classifier"
        );

        clean_queue(&pool).await;
    });
}

/// The posture that matters: with classification unavailable, a held comment stays
/// held. Nothing publishes on a failure.
#[test]
fn a_pending_comment_stays_pending_when_classification_is_unavailable() {
    serial(async {
        let pool = pool().await;
        clean_queue(&pool).await;

        let comment_id = seed_comment(&pool, STATUS_PENDING).await;
        let disp = dispatcher();

        // Queue the job the way the plugin does.
        sqlx::query(
            "INSERT INTO plugin_queue (plugin_name, queue_name, payload, created_at) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(PLUGIN)
        .bind(QUEUE)
        .bind(serde_json::json!({
            "comment_id": comment_id,
            "body": "Buy cheap watches at example.com",
            "author_id": uuid::Uuid::nil(),
            "item_id": uuid::Uuid::now_v7(),
        }))
        .bind(chrono::Utc::now().timestamp())
        .execute(&pool)
        .await
        .expect("queue a classification job");

        let cron = cron_with(pool.clone(), disp.clone());
        let stats = cron.drain_plugin_queues().await.expect("drain");

        assert_eq!(
            comment_status(&pool, comment_id).await,
            STATUS_PENDING,
            "with no provider, the comment must be left exactly where it was"
        );
        assert_eq!(
            stats.succeeded, 0,
            "a classification that could not happen is not a success: {stats:?}"
        );

        // And the work is not lost: the attempt was counted and the job is still
        // there to retry.
        let jobs = queued_jobs(&pool).await;
        assert_eq!(jobs.len(), 1, "the job must survive to be retried");
        assert!(
            jobs[0].2 >= 1,
            "the failed attempt must be counted, attempts were {}",
            jobs[0].2
        );

        clean_queue(&pool).await;
    });
}

/// The same for a published comment: a failed classification does not retroactively
/// hide content.
#[test]
fn a_published_comment_is_untouched_when_classification_is_unavailable() {
    serial(async {
        let pool = pool().await;
        clean_queue(&pool).await;

        let comment_id = seed_comment(&pool, STATUS_PUBLISHED).await;
        let disp = dispatcher();

        sqlx::query(
            "INSERT INTO plugin_queue (plugin_name, queue_name, payload, created_at) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(PLUGIN)
        .bind(QUEUE)
        .bind(serde_json::json!({ "comment_id": comment_id, "body": "A real remark" }))
        .bind(chrono::Utc::now().timestamp())
        .execute(&pool)
        .await
        .expect("queue a classification job");

        let cron = cron_with(pool.clone(), disp.clone());
        cron.drain_plugin_queues().await.expect("drain");

        assert_eq!(
            comment_status(&pool, comment_id).await,
            STATUS_PUBLISHED,
            "a failed classification must not unpublish anything"
        );

        clean_queue(&pool).await;
    });
}

/// A job that can never succeed is dropped rather than retried forever.
#[test]
fn a_job_with_no_comment_id_is_not_retried() {
    serial(async {
        let pool = pool().await;
        clean_queue(&pool).await;

        sqlx::query(
            "INSERT INTO plugin_queue (plugin_name, queue_name, payload, created_at) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(PLUGIN)
        .bind(QUEUE)
        .bind(serde_json::json!({ "body": "orphaned job" }))
        .bind(chrono::Utc::now().timestamp())
        .execute(&pool)
        .await
        .expect("queue a malformed job");

        let cron = cron_with(pool.clone(), dispatcher());
        cron.drain_plugin_queues().await.expect("drain");

        assert!(
            queued_jobs(&pool).await.is_empty(),
            "a job that can never succeed must be dropped, not retried"
        );

        clean_queue(&pool).await;
    });
}

/// The WASM-2 claim the design rests on: a kernel table named in `db_tables` is
/// inside the plugin's effective allowlist, and nothing else is.
///
/// The plugin owns no migrations, so `comment` is in the allowlist only because the
/// manifest says so. If that were not true, the plugin would need `raw_sql = true`
/// — the wide, unchecked escape hatch — to write one column.
#[test]
fn the_manifest_puts_only_the_comment_table_in_reach() {
    let dir = plugins_dir().join(PLUGIN);
    let info = PluginInfo::parse(&dir.join(format!("{PLUGIN}.info.toml"))).expect("parse manifest");
    let policy = DbPolicy::derive(&info, &dir);

    assert!(
        policy.check_table("comment").is_ok(),
        "a kernel table declared in db_tables must be reachable"
    );

    for other in ["users", "item", "site_config", "plugin_queue"] {
        assert!(
            policy.check_table(other).is_err(),
            "{other} is not declared and must be out of reach"
        );
    }

    assert!(
        policy.check_raw_sql().is_err(),
        "this plugin must not hold raw SQL: the structured update is enough, and \
         raw SQL would weaken the table guarantee"
    );
}

/// The manifest declares exactly the interfaces the flow needs, and no more.
#[test]
fn the_manifest_declares_the_interfaces_the_flow_needs() {
    let dir = plugins_dir().join(PLUGIN);
    let info = PluginInfo::parse(&dir.join(format!("{PLUGIN}.info.toml"))).expect("parse manifest");
    let capabilities = info.capabilities.as_ref().expect("declared capabilities");

    for needed in ["queue", "ai-api", "db", "logging"] {
        assert!(
            capabilities.host_interfaces.iter().any(|i| i == needed),
            "{needed} is required by the classification flow"
        );
    }
    assert!(
        capabilities.ai_background,
        "the classify call happens outside a web request, so it needs the \
         background-AI capability"
    );
    assert!(
        !capabilities.host_interfaces.iter().any(|i| i == "http"),
        "the plugin makes no HTTP calls of its own"
    );
}
