#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Argus M3 integration: the reader surface, admin management, and the
//! configuration move onto Items.
//!
//! Drives the **real** `plugins/argus` wasm through the real `TapDispatcher`,
//! the real `GatherService`, and a real Postgres. What is asserted here is the
//! part M1 and M2 had no reason to exercise: that an admin's feed edit reaches
//! the pipeline, that a reader's view is recorded, that the story gathers order
//! and filter as claimed, and that the permission strings the seeded roles carry
//! are the ones the kernel checks.
//!
//! Build the wasm first, as for `argus_pipeline_test`:
//!
//! ```text
//! cargo build -p argus --target wasm32-wasip1 --release \
//!   && cp target/wasm32-wasip1/release/argus.wasm plugins/argus/
//! ```

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use sqlx::PgPool;
use uuid::Uuid;

use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;

use trovato_kernel::content::{ContentTypeRegistry, RecordTypeRegistry};

use trovato_kernel::gather::{
    CategoryService, GatherExtensionRegistry, GatherService, QueryContext,
};
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::tap::{RequestServices, RequestState, TapDispatcher, TapRegistry, UserContext};

const PLUGIN: &str = "argus";
const LIVE_STAGE: &str = "0193a5a0-0000-7000-8000-000000000001";

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

async fn fresh_pool() -> PgPool {
    trovato_test_utils::env::load_dotenv();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:5432/trovato".to_string());
    let pool = PgPool::connect(&url).await.expect("connect test DB");
    trovato_kernel::db::run_migrations(&pool)
        .await
        .expect("run kernel migrations");
    for migration in [
        "001_argus_schema.sql",
        // M1's gathers, so the article list this suite checks is registered.
        "002_argus_gather.sql",
        "003_argus_intelligence.sql",
        "004_argus_reader.sql",
    ] {
        let sql =
            std::fs::read_to_string(plugins_dir().join(format!("{PLUGIN}/migrations/{migration}")))
                .unwrap_or_else(|e| panic!("read {migration}: {e}"));
        sqlx::raw_sql(&sql)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("apply {migration}: {e}"));
    }
    ContentTypeRegistry::new(pool.clone(), std::time::Duration::from_secs(60))
        .sync_from_plugins(&dispatcher())
        .await
        .expect("register argus content types");
    pool
}

async fn reset(pool: &PgPool) {
    for stmt in [
        "DELETE FROM plugin_queue WHERE plugin_name = 'argus'",
        "DELETE FROM item WHERE type IN ('argus_story', 'argus_feed', 'argus_topic')",
        "DELETE FROM argus_read_state",
        "DELETE FROM argus_reactions",
        "DELETE FROM argus_subscriptions",
        "TRUNCATE argus_articles",
        "TRUNCATE argus_feeds",
        "TRUNCATE argus_topics",
        "TRUNCATE argus_state",
        "TRUNCATE argus_stories",
    ] {
        sqlx::query(stmt).execute(pool).await.unwrap();
    }
}

/// Decode a view tap's output into the HTML it meant to emit.
///
/// `#[plugin_tap]` JSON-serializes a `String` return, and the item route appends
/// that serialized form to the page verbatim — so what the kernel actually shows
/// a reader is this string *with quotes around it*
/// (`G-VIEW-OUTPUT-JSON-ENCODED`). Tests decode so they assert on the markup the
/// plugin built; `the_view_output_is_json_encoded_by_the_contract` pins the
/// defect itself.
fn decode_view(raw: &str) -> String {
    serde_json::from_str::<String>(raw)
        .unwrap_or_else(|e| panic!("view output was not a JSON string ({e}): {raw}"))
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn background(pool: &PgPool) -> RequestState {
    let disp = dispatcher();
    RequestState::new(
        UserContext::background(),
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
            .with_plugin_runtime(disp.runtime().clone()),
    )
}

/// A request state carrying an authenticated viewer, as `load_for_view` builds
/// for `tap_item_view`.
fn as_viewer(pool: &PgPool, user_id: Uuid) -> RequestState {
    let disp = dispatcher();
    RequestState::new(
        UserContext::authenticated(user_id, vec!["access content".to_string()]),
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
            .with_plugin_runtime(disp.runtime().clone()),
    )
}

/// A real, non-anonymous reader.
///
/// Deliberately not "the first user": a fresh install's oldest user is the
/// **anonymous** account, whose id is the nil uuid — which is exactly what
/// `tap_item_view` declines to record read state against. Using it would make
/// this suite assert that the anonymous guard works while claiming to test an
/// authenticated reader.
async fn reader(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, name, pass, mail, is_admin, status) \
         VALUES ($1, $2, 'x', $3, false, 1)",
    )
    .bind(id)
    .bind(format!("argus-reader-{id}"))
    .bind(format!("{id}@example.test"))
    .execute(pool)
    .await
    .unwrap();
    id
}

/// Any user id, for a column that only needs to satisfy a foreign key.
async fn any_user(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users ORDER BY created LIMIT 1")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Insert an Item directly, returning its id.
async fn seed_item(
    pool: &PgPool,
    item_type: &str,
    title: &str,
    fields: serde_json::Value,
    published: bool,
) -> Uuid {
    let id = Uuid::now_v7();
    let author = any_user(pool).await;
    sqlx::query(
        "INSERT INTO item (id, type, title, author_id, status, created, changed, \
                           promote, sticky, fields, stage_id, language, item_group_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $6, 0, 0, $7, $8::uuid, 'en', $1)",
    )
    .bind(id)
    .bind(item_type)
    .bind(title)
    .bind(author)
    .bind(if published { 1i16 } else { 0i16 })
    .bind(now())
    .bind(&fields)
    .bind(LIVE_STAGE)
    .execute(pool)
    .await
    .unwrap();
    id
}

/// Seed a story Item in the `{"value": …}` field shape the M2 sync writes.
async fn seed_story(
    pool: &PgPool,
    title: &str,
    topic: Option<Uuid>,
    active: bool,
    changed: i64,
) -> Uuid {
    let mut fields = serde_json::json!({
        "field_summary": { "value": format!("Synthesis for {title}.") },
        "field_is_active": { "value": active },
        "field_article_count": { "value": 2 },
    });
    if let Some(t) = topic {
        fields["field_topic_id"] = serde_json::json!({ "value": t.to_string() });
    }
    let id = seed_item(pool, "argus_story", title, fields, true).await;
    sqlx::query("UPDATE item SET changed = $2 WHERE id = $1")
        .bind(id)
        .bind(changed)
        .execute(pool)
        .await
        .unwrap();
    id
}

/// A gather service wired the way the HTTP route wires it, minus the pieces
/// these queries do not touch (AI providers, the vector store).
/// An `ItemService` wired the way the running kernel wires it, so
/// `load_for_view` dispatches the plugin's view tap for real.
fn item_service(pool: &PgPool) -> Arc<trovato_kernel::content::ItemService> {
    let disp = dispatcher();
    let services =
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
            .with_plugin_runtime(disp.runtime().clone());
    Arc::new(trovato_kernel::content::ItemService::new(
        pool.clone(),
        disp,
        services,
        Duration::from_secs(60),
        None,
        None,
    ))
}

async fn gather(pool: &PgPool) -> Arc<GatherService> {
    let categories = CategoryService::new(pool.clone(), Duration::from_secs(60));
    let svc = GatherService::new(
        pool.clone(),
        categories,
        Arc::new(GatherExtensionRegistry::new()),
        Duration::from_secs(60),
        100,
        None,
        None,
    );
    // The article gather targets a record type, so the registry has to be
    // wired the way the running kernel wires it.
    let runtime = dispatcher().runtime().clone();
    let compiled = runtime.get_plugin(PLUGIN).expect("plugin loaded");
    let (records, _errors) = RecordTypeRegistry::build(
        [(
            PLUGIN,
            compiled.db_policy().as_ref(),
            compiled.info.record_types.as_slice(),
        )],
        &HashSet::new(),
    );
    svc.set_record_types(Arc::new(records));
    svc.load_queries().await.expect("load gather queries");
    svc
}

/// Run a registered gather with exposed-filter values supplied, as the exposed
/// filter form does.
async fn gather_ids_with_exposed(
    svc: &Arc<GatherService>,
    query_id: &str,
    exposed: &[(&str, &str)],
) -> Vec<String> {
    let mut filters = HashMap::new();
    for (k, v) in exposed {
        filters.insert(
            (*k).to_string(),
            trovato_kernel::gather::FilterValue::String((*v).to_string()),
        );
    }
    let result = svc
        .execute(
            query_id,
            1,
            filters,
            Uuid::parse_str(LIVE_STAGE).unwrap(),
            &QueryContext::default(),
        )
        .await
        .unwrap_or_else(|e| panic!("{query_id} failed: {e:#}"));
    result
        .items
        .iter()
        .filter_map(|i| i.get("id").and_then(|v| v.as_str()).map(str::to_string))
        .collect()
}

/// Run a registered gather against the Live stage and return the row ids.
async fn gather_ids(
    svc: &Arc<GatherService>,
    query_id: &str,
    url_args: &[(&str, &str)],
) -> Vec<String> {
    let mut context = QueryContext::default();
    for (k, v) in url_args {
        context.url_args.insert((*k).to_string(), (*v).to_string());
    }
    let result = svc
        .execute(
            query_id,
            1,
            HashMap::new(),
            Uuid::parse_str(LIVE_STAGE).unwrap(),
            &context,
        )
        .await
        .unwrap_or_else(|e| panic!("{query_id} failed: {e:#}"));
    result
        .items
        .iter()
        .filter_map(|i| i.get("id").and_then(|v| v.as_str()).map(str::to_string))
        .collect()
}

// ===========================================================================
// Configuration on Items
// ===========================================================================

#[test]
fn an_admin_edit_reaches_the_scheduler_on_the_next_tick() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let topic = seed_item(
            &pool,
            "argus_topic",
            "Infrastructure",
            serde_json::json!({
                "field_relevance_prompt": "Datacentre news",
                "field_relevance_threshold": 60,
            }),
            true,
        )
        .await;
        let feed = seed_item(
            &pool,
            "argus_feed",
            "Example",
            serde_json::json!({
                "field_url": "https://example.test/feed.xml",
                "field_topic": topic.to_string(),
                "field_fetch_interval": 900,
            }),
            true,
        )
        .await;

        let disp = dispatcher();
        let input = serde_json::json!({ "timestamp": now() }).to_string();
        disp.dispatch_to_plugin("tap_cron", &input, PLUGIN, background(&pool))
            .await
            .expect("tap_cron");

        let jobs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM plugin_queue WHERE plugin_name = 'argus'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(jobs, 1, "the published feed Item was scheduled");

        // Unpublishing the Item is how an admin pauses a feed. The next tick
        // must not schedule it — this is the whole "admin edits a feed and the
        // pipeline picks it up" claim, and its latency is one cron cycle.
        sqlx::query("DELETE FROM plugin_queue WHERE plugin_name = 'argus'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE item SET status = 0 WHERE id = $1")
            .bind(feed)
            .execute(&pool)
            .await
            .unwrap();

        let input = serde_json::json!({ "timestamp": now() + 3_600 }).to_string();
        disp.dispatch_to_plugin("tap_cron", &input, PLUGIN, background(&pool))
            .await
            .expect("tap_cron");

        let jobs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM plugin_queue WHERE plugin_name = 'argus'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(jobs, 0, "an unpublished feed is not scheduled");
    });
}

#[test]
fn a_feed_with_an_unusable_url_is_never_scheduled() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        // presave blanks a URL it cannot use; the scheduler is what refuses to
        // poll it, because presave cannot unpublish an Item (G-NO-PRESAVE-VETO).
        seed_item(
            &pool,
            "argus_feed",
            "Broken",
            serde_json::json!({ "field_url": "", "field_fetch_interval": 900 }),
            true,
        )
        .await;

        let disp = dispatcher();
        let input = serde_json::json!({ "timestamp": now() }).to_string();
        disp.dispatch_to_plugin("tap_cron", &input, PLUGIN, background(&pool))
            .await
            .expect("tap_cron");

        let jobs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM plugin_queue WHERE plugin_name = 'argus'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(jobs, 0);
    });
}

#[test]
fn presave_clamps_an_impolite_interval_and_records_why() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let disp = dispatcher();
        let input = serde_json::json!({
            "item_type": "argus_feed",
            "title": "Hammering",
            "status": 1,
            "fields": {
                "field_url": "  https://example.test/feed.xml  ",
                "field_topic": "019a4720-0000-7000-8000-0000000000c1",
                "field_fetch_interval": 5,
            }
        })
        .to_string();
        let out = disp
            .dispatch_to_plugin("tap_item_presave", &input, PLUGIN, background(&pool))
            .await
            .expect("tap_item_presave");
        let out: serde_json::Value = serde_json::from_str(&out.output).unwrap();

        assert_eq!(
            out["fields"]["field_fetch_interval"], 300,
            "clamped to the five-minute floor"
        );
        assert_eq!(
            out["fields"]["field_url"], "https://example.test/feed.xml",
            "trimmed"
        );
        assert!(
            out["fields"]["field_config_note"]
                .as_str()
                .unwrap()
                .contains("clamped"),
            "the admin is told what changed: {out}"
        );
    });
}

#[test]
fn presave_clamps_a_topic_threshold_out_of_range() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let disp = dispatcher();
        let input = serde_json::json!({
            "item_type": "argus_topic",
            "title": "Everything",
            "status": 1,
            "fields": {
                "field_relevance_prompt": "Anything at all",
                "field_relevance_threshold": 500,
            }
        })
        .to_string();
        let out = disp
            .dispatch_to_plugin("tap_item_presave", &input, PLUGIN, background(&pool))
            .await
            .expect("tap_item_presave");
        let out: serde_json::Value = serde_json::from_str(&out.output).unwrap();
        assert_eq!(out["fields"]["field_relevance_threshold"], 100);
    });
}

#[test]
fn presave_leaves_other_content_types_alone() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let disp = dispatcher();
        let input = serde_json::json!({
            "item_type": "blog",
            "title": "Not ours",
            "fields": { "field_url": "nonsense" }
        })
        .to_string();
        let out = disp
            .dispatch_to_plugin("tap_item_presave", &input, PLUGIN, background(&pool))
            .await
            .expect("tap_item_presave");
        let out: serde_json::Value = serde_json::from_str(&out.output).unwrap();
        assert!(
            out.get("fields").is_none(),
            "argus must not rewrite another plugin's item: {out}"
        );
    });
}

#[test]
fn deleting_a_feed_item_retires_its_state_row() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let feed = seed_item(
            &pool,
            "argus_feed",
            "Doomed",
            serde_json::json!({ "field_url": "https://example.test/f.xml" }),
            true,
        )
        .await;
        sqlx::query("INSERT INTO argus_feeds (id, last_fetched_at, created, changed) VALUES ($1, $2, $2, $2)")
            .bind(feed)
            .bind(now())
            .execute(&pool)
            .await
            .unwrap();

        let disp = dispatcher();
        let input = serde_json::json!({ "id": feed.to_string(), "type": "argus_feed" }).to_string();
        disp.dispatch_to_plugin("tap_item_delete", &input, PLUGIN, background(&pool))
            .await
            .expect("tap_item_delete");

        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM argus_feeds WHERE id = $1")
            .bind(feed)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0, "the orphaned fetch state is gone");
    });
}

// ===========================================================================
// The one-shot backfill
// ===========================================================================

#[test]
fn the_backfill_carries_legacy_rows_onto_items_exactly_once() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        // An M1/M2-shaped install: configuration in the plugin tables, articles
        // referencing those ids.
        let legacy_topic = Uuid::now_v7();
        let legacy_feed = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO argus_topics (id, name, relevance_prompt, relevance_threshold, enabled, created, changed) \
             VALUES ($1, 'Legacy topic', 'Is this about AI?', 70, true, $2, $2)",
        )
        .bind(legacy_topic)
        .bind(now())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO argus_feeds (id, url, name, topic_id, fetch_interval_seconds, enabled, created, changed) \
             VALUES ($1, 'https://legacy.test/feed.xml', 'Legacy feed', $2, 1800, true, $3, $3)",
        )
        .bind(legacy_feed)
        .bind(legacy_topic)
        .bind(now())
        .execute(&pool)
        .await
        .unwrap();
        let article = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO argus_articles (id, url, title, content, feed_id, topic_id, pipeline_state, created, changed) \
             VALUES ($1, 'https://legacy.test/a', 'A', 'body', $2, $3, 'fetched', $4, $4)",
        )
        .bind(article)
        .bind(legacy_feed)
        .bind(legacy_topic)
        .bind(now())
        .execute(&pool)
        .await
        .unwrap();

        let disp = dispatcher();
        let input = serde_json::json!({ "timestamp": now() }).to_string();
        let out = disp
            .dispatch_to_plugin("tap_cron", &input, PLUGIN, background(&pool))
            .await
            .expect("tap_cron");
        let out: serde_json::Value = serde_json::from_str(&out.output).unwrap();
        assert_eq!(out["config_backfill"]["topics"], 1);
        assert_eq!(out["config_backfill"]["feeds"], 1);

        let feeds: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE type = 'argus_feed'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let topics: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE type = 'argus_topic'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((feeds, topics), (1, 1), "one Item per legacy row");

        // The article was repointed at the new ids, so nothing dangles.
        let (feed_id, topic_id): (Uuid, Uuid) =
            sqlx::query_as("SELECT feed_id, topic_id FROM argus_articles WHERE id = $1")
                .bind(article)
                .fetch_one(&pool)
                .await
                .unwrap();
        let new_feed: Uuid = sqlx::query_scalar("SELECT id FROM item WHERE type = 'argus_feed'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let new_topic: Uuid = sqlx::query_scalar("SELECT id FROM item WHERE type = 'argus_topic'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(feed_id, new_feed);
        assert_eq!(topic_id, new_topic);

        // The feed's configuration survived the move intact.
        let fields: serde_json::Value = sqlx::query_scalar("SELECT fields FROM item WHERE id = $1")
            .bind(new_feed)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(fields["field_url"], "https://legacy.test/feed.xml");
        assert_eq!(fields["field_fetch_interval"], 1800);
        assert_eq!(fields["field_topic"], new_topic.to_string());

        // A second tick must not duplicate anything — the marker short-circuits.
        let input = serde_json::json!({ "timestamp": now() + 60 }).to_string();
        let out = disp
            .dispatch_to_plugin("tap_cron", &input, PLUGIN, background(&pool))
            .await
            .expect("tap_cron");
        let out: serde_json::Value = serde_json::from_str(&out.output).unwrap();
        assert_eq!(out["config_backfill"]["already_done"], true);

        let feeds: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE type = 'argus_feed'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(feeds, 1, "the backfill did not run twice");
    });
}

// ===========================================================================
// The story page
// ===========================================================================

#[test]
fn viewing_a_story_renders_its_sources_and_records_read_state() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let user = reader(&pool).await;
        let story = seed_story(&pool, "A clustered event", None, true, now()).await;
        let sources = serde_json::json!([
            { "source": "Ars Technica", "title": "Original report", "contribution": "member" },
            { "source": "The Verge", "title": "Same report", "contribution": "duplicate" },
        ])
        .to_string();
        sqlx::query(
            "UPDATE item SET fields = fields || jsonb_build_object( \
                 'field_sources', jsonb_build_object('value', $2::text), \
                 'field_entities', jsonb_build_object('value', $3::text)) \
             WHERE id = $1",
        )
        .bind(story)
        .bind(&sources)
        .bind(serde_json::json!(["Acme Corp", "Jane Roe"]).to_string())
        .execute(&pool)
        .await
        .unwrap();

        let item: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(i) - 'search_vector' FROM item i WHERE i.id = $1")
                .bind(story)
                .fetch_one(&pool)
                .await
                .unwrap();

        let disp = dispatcher();
        let html = disp
            .dispatch_to_plugin(
                "tap_item_view",
                &item.to_string(),
                PLUGIN,
                as_viewer(&pool, user),
            )
            .await
            .expect("tap_item_view")
            .output;
        let html = decode_view(&html);

        assert!(html.contains("Ars Technica"), "credited source: {html}");
        assert!(html.contains("argus-story__source--duplicate"), "{html}");
        assert!(html.contains("Acme Corp"), "top entity: {html}");
        assert!(html.contains("Synthesis for"), "summary: {html}");
        assert!(
            html.contains(&format!("data-comments-for='{story}'")),
            "comment mount: {html}"
        );

        let (views, first, last): (i64, i64, i64) = sqlx::query_as(
            "SELECT view_count, first_seen_at, last_seen_at FROM argus_read_state \
             WHERE user_id = $1 AND story_item_id = $2",
        )
        .bind(user)
        .bind(story)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(views, 1);
        assert_eq!(first, last, "one view stamps both ends the same");

        // A second view advances the count without moving first_seen_at.
        disp.dispatch_to_plugin(
            "tap_item_view",
            &item.to_string(),
            PLUGIN,
            as_viewer(&pool, user),
        )
        .await
        .expect("tap_item_view");
        let (views, first2): (i64, i64) = sqlx::query_as(
            "SELECT view_count, first_seen_at FROM argus_read_state \
             WHERE user_id = $1 AND story_item_id = $2",
        )
        .bind(user)
        .bind(story)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(views, 2);
        assert_eq!(first, first2, "first_seen_at never moves");
    });
}

#[test]
fn an_anonymous_view_records_no_read_state() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let story = seed_story(&pool, "Public story", None, true, now()).await;
        let item: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(i) - 'search_vector' FROM item i WHERE i.id = $1")
                .bind(story)
                .fetch_one(&pool)
                .await
                .unwrap();

        let disp = dispatcher();
        let state = RequestState::new(
            UserContext::anonymous(),
            RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
                .with_plugin_runtime(disp.runtime().clone()),
        );
        let html = disp
            .dispatch_to_plugin("tap_item_view", &item.to_string(), PLUGIN, state)
            .await
            .expect("tap_item_view")
            .output;
        let html = decode_view(&html);

        assert!(html.contains("argus-story"), "the page still renders");
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM argus_read_state")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0, "nothing is recorded against an anonymous reader");
    });
}

#[test]
fn a_non_story_item_gets_no_argus_fragment() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let disp = dispatcher();
        let input = serde_json::json!({
            "id": Uuid::now_v7().to_string(),
            "type": "blog",
            "title": "Someone else's post",
            "fields": {}
        })
        .to_string();
        let html = disp
            .dispatch_to_plugin("tap_item_view", &input, PLUGIN, background(&pool))
            .await
            .expect("tap_item_view")
            .output;
        let html = decode_view(&html);
        assert!(html.trim().is_empty(), "got: {html}");
    });
}

/// **Flipped by K1 fix 3.** This test used to pin G-VIEW-OUTPUT-JSON-ENCODED:
/// the tap macro's JSON envelope reached the page undecoded, so a reader saw a
/// stray `"` at each end of every plugin fragment and a backslash inside every
/// double-quoted attribute. `ItemService::load_for_view` now decodes the
/// envelope, so what the route appends is the markup the plugin built.
#[test]
fn the_view_output_is_decoded_before_it_reaches_the_page() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let story = seed_story(&pool, "Encoding", None, true, now()).await;
        let item: serde_json::Value = sqlx::query_scalar(
            "SELECT to_jsonb(i) - \'search_vector\' FROM item i WHERE i.id = $1",
        )
        .bind(story)
        .fetch_one(&pool)
        .await
        .unwrap();

        let disp = dispatcher();
        let raw = disp
            .dispatch_to_plugin(
                "tap_item_view",
                &item.to_string(),
                PLUGIN,
                background(&pool),
            )
            .await
            .expect("tap_item_view")
            .output;

        // The wire form is unchanged and still frozen: `#[plugin_tap]`
        // JSON-serializes a `String` return, so the dispatcher hands back a
        // JSON string literal. The fix is on the kernel's side of that wire,
        // not a change to what plugins emit — which is what makes it additive.
        assert!(raw.starts_with('"') && raw.ends_with('"'), "got: {raw}");

        // What the page gets is now the decoded fragment.
        let decoded = trovato_kernel::content::decode_view_output(&raw);
        assert!(
            !decoded.starts_with('"') && !decoded.ends_with('"'),
            "the page must not receive the JSON envelope: {decoded}"
        );
        assert!(
            decoded.contains("argus-story"),
            "expected the story fragment, got: {decoded}"
        );

        // And the decode is lossless for a fragment carrying characters serde
        // must escape — the mitigation Argus adopted (single-quoted attributes)
        // is no longer load-bearing, so prove the general case rather than only
        // the escape-free one this plugin happens to emit.
        let quoted = r#"<div class="a" data-x="1 &amp; 2">back\slash</div>"#;
        assert_eq!(
            trovato_kernel::content::decode_view_output(
                &serde_json::to_string(quoted).expect("encode")
            ),
            quoted,
            "a fragment full of serde escapes must round-trip intact"
        );

        // Whole-route proof: what `load_for_view` collects is the markup, not
        // the envelope. This is the value `routes/item.rs` pushes onto the page.
        let items = item_service(&pool);
        let user = UserContext::authenticated(reader(&pool).await, vec!["access content".into()]);
        let (_item, outputs) = items
            .load_for_view(story, &user)
            .await
            .expect("load_for_view")
            .expect("story is viewable");
        assert!(
            outputs.iter().any(|o| o.contains("argus-story")),
            "load_for_view must yield decoded markup, got: {outputs:?}"
        );
        assert!(
            outputs.iter().all(|o| !o.starts_with('"')),
            "no collected output may still carry the JSON envelope: {outputs:?}"
        );
    });
}

// ===========================================================================
// Reader state storage
// ===========================================================================

#[test]
fn the_reaction_triple_is_unique_per_reader_story_and_kind() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let user = reader(&pool).await;
        let story = seed_story(&pool, "Reacted to", None, true, now()).await;

        for _ in 0..2 {
            sqlx::query(
                "INSERT INTO argus_reactions (user_id, story_item_id, reaction_type, created) \
                 VALUES ($1, $2, 'upvote', $3) ON CONFLICT DO NOTHING",
            )
            .bind(user)
            .bind(story)
            .bind(now())
            .execute(&pool)
            .await
            .unwrap();
        }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM argus_reactions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "a replayed reaction does not double-count");

        // A different kind from the same reader on the same story is a
        // different row, which is what makes bookmark-and-upvote expressible.
        sqlx::query(
            "INSERT INTO argus_reactions (user_id, story_item_id, reaction_type, created) \
             VALUES ($1, $2, 'bookmark', $3)",
        )
        .bind(user)
        .bind(story)
        .bind(now())
        .execute(&pool)
        .await
        .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM argus_reactions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 2);
    });
}

#[test]
fn a_held_reaction_renders_on_the_story_page() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let user = reader(&pool).await;
        let story = seed_story(&pool, "Bookmarked", None, true, now()).await;
        sqlx::query(
            "INSERT INTO argus_reactions (user_id, story_item_id, reaction_type, created) \
             VALUES ($1, $2, 'bookmark', $3)",
        )
        .bind(user)
        .bind(story)
        .bind(now())
        .execute(&pool)
        .await
        .unwrap();

        let item: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(i) - 'search_vector' FROM item i WHERE i.id = $1")
                .bind(story)
                .fetch_one(&pool)
                .await
                .unwrap();

        let disp = dispatcher();
        let html = disp
            .dispatch_to_plugin(
                "tap_item_view",
                &item.to_string(),
                PLUGIN,
                as_viewer(&pool, user),
            )
            .await
            .expect("tap_item_view")
            .output;
        let html = decode_view(&html);
        assert!(
            html.contains("argus-story__reaction--bookmark"),
            "the reader's own state is shown back to them: {html}"
        );
    });
}

// ===========================================================================
// Gathers
// ===========================================================================

#[test]
fn the_story_feed_returns_active_stories_newest_first() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let base = now();
        let older = seed_story(&pool, "Older story", None, true, base - 3_600).await;
        let newer = seed_story(&pool, "Newer story", None, true, base).await;
        let retired = seed_story(&pool, "Retired story", None, false, base + 60).await;

        let svc = gather(&pool).await;
        let ids = gather_ids(&svc, "argus_story_list", &[]).await;
        assert_eq!(
            ids,
            vec![newer.to_string(), older.to_string()],
            "active stories, newest first; the retired one is excluded \
             (retired id {retired})"
        );
    });
}

#[test]
fn the_archive_returns_only_retired_stories() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let base = now();
        seed_story(&pool, "Active", None, true, base).await;
        let retired = seed_story(&pool, "Retired", None, false, base - 60).await;

        let svc = gather(&pool).await;
        let ids = gather_ids(&svc, "argus_story_archive", &[]).await;
        assert_eq!(ids, vec![retired.to_string()]);
    });
}

#[test]
fn stories_by_topic_filters_on_the_url_argument() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let wanted = Uuid::now_v7();
        let other = Uuid::now_v7();
        let base = now();
        let hit = seed_story(&pool, "Wanted topic", Some(wanted), true, base).await;
        seed_story(&pool, "Other topic", Some(other), true, base).await;

        let svc = gather(&pool).await;
        let ids = gather_ids(
            &svc,
            "argus_stories_by_topic",
            &[("topic", &wanted.to_string())],
        )
        .await;
        assert_eq!(ids, vec![hit.to_string()], "only the requested topic");
    });
}

/// **Flipped by K1 fix 5.** This test used to pin G-EXPOSED-FILTER-NO-MATCH-ALL:
/// M1 shipped `argus_article_list` with an exposed `equals` filter defaulting to
/// `""`, and an unanswered exposed filter kept that default, so the query
/// builder emitted `topic_id = ''` — which over a real `uuid` column raised
/// `invalid input syntax for type uuid: ""`. The `/articles` route therefore
/// served a **500 in its own default state**. An unanswered exposed filter is
/// now dropped, so the default state is the whole list.
#[test]
fn m1s_article_gather_returns_everything_while_its_topic_filter_is_blank() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let topic = Uuid::now_v7();
        let article = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO argus_articles (id, url, title, content, topic_id, relevance_score, \
                                         pipeline_state, created, changed) \
             VALUES ($1, \'https://x.test/1\', \'Kept\', \'body\', $2, 90, \'decided\', $3, $3)",
        )
        .bind(article)
        .bind(topic)
        .bind(now())
        .execute(&pool)
        .await
        .unwrap();

        let svc = gather(&pool).await;
        let result = svc
            .execute(
                "argus_article_list",
                1,
                HashMap::new(),
                Uuid::parse_str(LIVE_STAGE).unwrap(),
                &QueryContext::default(),
            )
            .await
            .expect("the blank default state must render, not 500");
        assert_eq!(
            result.items.len(),
            1,
            "a blank exposed filter matches all, got {:?}",
            result.items
        );

        // Supplying the value still narrows.
        let ids = gather_ids_with_exposed(
            &svc,
            "argus_article_list",
            &[("topic_id", &topic.to_string())],
        )
        .await;
        assert_eq!(ids.len(), 1, "with a topic supplied the article is found");

        // And a topic nobody wrote returns an empty list rather than everything.
        let ids = gather_ids_with_exposed(
            &svc,
            "argus_article_list",
            &[("topic_id", &Uuid::now_v7().to_string())],
        )
        .await;
        assert!(ids.is_empty(), "an answered filter still excludes");
    });
}

// ===========================================================================
// Roles and permissions
// ===========================================================================

#[test]
fn the_seeded_roles_carry_the_permission_strings_the_kernel_checks() {
    serial(async {
        let pool = fresh_pool().await;

        // The strings must match what PermissionDefinition::crud_for_type
        // generates and what the item routes build, or the role grants nothing.
        for (role, permission) in [
            ("argus_admin", "administer argus"),
            ("argus_admin", "create argus_feed content"),
            ("argus_admin", "edit argus_topic content"),
            ("argus_reader", "view argus stories"),
            ("argus_reader", "react to argus stories"),
        ] {
            let found: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM role_permissions rp \
                 JOIN roles r ON r.id = rp.role_id \
                 WHERE r.name = $1 AND rp.permission = $2",
            )
            .bind(role)
            .bind(permission)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(found, 1, "{role} is missing {permission:?}");
        }

        // A reader is not an administrator.
        let leaked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM role_permissions rp \
             JOIN roles r ON r.id = rp.role_id \
             WHERE r.name = 'argus_reader' AND rp.permission LIKE '%argus_feed%'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(leaked, 0, "argus_reader must not manage feeds");
    });
}

#[test]
fn the_plugin_declares_every_permission_the_roles_grant() {
    serial(async {
        let pool = fresh_pool().await;

        let disp = dispatcher();
        let declared = disp
            .dispatch_to_plugin("tap_perm", "{}", PLUGIN, background(&pool))
            .await
            .expect("tap_perm");
        let declared: serde_json::Value = serde_json::from_str(&declared.output).unwrap();
        let declared: Vec<String> = declared
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .collect();

        let granted: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT rp.permission FROM role_permissions rp \
             JOIN roles r ON r.id = rp.role_id \
             WHERE r.name IN ('argus_admin', 'argus_reader')",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        for permission in &granted {
            assert!(
                declared.contains(permission),
                "role_permissions grants {permission:?}, which tap_perm never \
                 declares — it would never appear on the permissions screen. \
                 Declared: {declared:?}"
            );
        }
    });
}
