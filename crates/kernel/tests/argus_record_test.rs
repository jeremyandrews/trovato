#![allow(clippy::unwrap_used, clippy::expect_used)]
//! argus-m1 M1-2 / M1-11 integration: the `argus_article` lightweight-record
//! type, exercised through a **real** `GatherService` over the **real**
//! `argus_articles` table with the **real** `plugins/argus` wasm loaded.
//!
//! Proves:
//!   - the manifest parses and `argus_article` is admitted by the record-type
//!     registry (validated against the migration-owned db allowlist) — M1-2;
//!   - a gather over the record type returns seeded rows with field access
//!     applied — M1-11;
//!   - a story→articles reverse reference resolves (gather filtered by the
//!     `story_id` record field) — M1-2.
//!
//! Requires Postgres. Build the plugin first:
//!   cargo build -p argus --target wasm32-wasip1 --release \
//!     && cp target/wasm32-wasip1/release/argus.wasm plugins/argus/

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use trovato_kernel::content::{ItemService, RecordTypeRegistry};
use trovato_kernel::gather::{
    CategoryService, GatherExtensionRegistry, GatherService, QueryContext, QueryDefinition,
    QueryDisplay,
};
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::tap::{RequestServices, TapDispatcher, TapRegistry, UserContext};
use uuid::Uuid;

const PLUGIN: &str = "argus";
const RECORD_TYPE: &str = "argus_article";

fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:5432/trovato".to_string())
}

/// Load the argus plugin, admit its record types, and wire a standalone
/// `GatherService` over the pool.
async fn wire(pool: sqlx::PgPool) -> Arc<GatherService> {
    let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
    runtime
        .load_plugin(&plugins_dir().join(PLUGIN))
        .unwrap_or_else(|e| {
            panic!(
                "failed to load '{PLUGIN}': {e:#}\n\
                 build it first: cargo build -p {PLUGIN} --target wasm32-wasip1 --release \
                 && cp target/wasm32-wasip1/release/{PLUGIN}.wasm plugins/{PLUGIN}/"
            )
        });
    let runtime = Arc::new(runtime);

    let compiled = runtime.get_plugin(PLUGIN).expect("plugin loaded");
    let (registry, errors) = RecordTypeRegistry::build(
        [(
            PLUGIN,
            compiled.db_policy().as_ref(),
            compiled.info.record_types.as_slice(),
        )],
        &HashSet::new(),
    );
    // M1-2: the manifest parses and the record type is admitted by the registry.
    assert!(errors.is_empty(), "record types rejected: {errors:?}");
    assert!(
        registry.contains(RECORD_TYPE),
        "argus_article not registered"
    );
    assert!(registry.contains("argus_feed"), "argus_feed not registered");
    assert!(
        registry.contains("argus_topic"),
        "argus_topic not registered"
    );

    let dispatcher = Arc::new(TapDispatcher::new(
        Arc::clone(&runtime),
        Arc::new(TapRegistry::from_plugins(&runtime)),
    ));
    let services =
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
            .with_plugin_runtime(Arc::clone(&runtime));
    let items = Arc::new(ItemService::new(
        pool.clone(),
        Arc::clone(&dispatcher),
        services,
        Duration::from_secs(60),
        None,
        None,
    ));
    let categories = CategoryService::new(pool.clone(), Duration::from_secs(60));
    let gather = GatherService::new(
        pool,
        categories,
        Arc::new(GatherExtensionRegistry::new()),
        trovato_kernel::gather::GatherConfig {
            ttl: Duration::from_secs(60),
            max_page_size: 100,
            access: trovato_kernel::gather::GatherAccessConfig::default(),
        },
        None,
        None,
    );
    gather.set_item_service(items);
    gather.set_record_types(Arc::new(registry));
    gather
}

/// Apply the argus schema migration and seed a controlled set of article rows.
async fn seed(pool: &sqlx::PgPool, story_a: Uuid, story_b: Uuid) {
    let migration = std::fs::read_to_string(
        plugins_dir().join(format!("{PLUGIN}/migrations/001_argus_schema.sql")),
    )
    .expect("read migration");
    sqlx::raw_sql(&migration)
        .execute(pool)
        .await
        .expect("create argus schema");
    sqlx::query("TRUNCATE argus_articles")
        .execute(pool)
        .await
        .expect("truncate");

    let topic = Uuid::now_v7();
    let feed = Uuid::now_v7();
    let now = 1_700_000_000_i64;
    // (url, title, score, state, story_id)
    let rows: [(&str, &str, i32, &str, Option<Uuid>); 3] = [
        ("https://x.test/1", "Kept One", 90, "decided", Some(story_a)),
        ("https://x.test/2", "Kept Two", 75, "decided", Some(story_a)),
        (
            "https://x.test/3",
            "Discarded",
            10,
            "discarded",
            Some(story_b),
        ),
    ];
    for (url, title, score, state, story) in rows {
        sqlx::query(
            "INSERT INTO argus_articles \
             (id, url, title, content, topic_id, feed_id, story_id, relevance_score, pipeline_state, created, changed) \
             VALUES (gen_random_uuid(), $1, $2, 'body', $3, $4, $5, $6, $7, $8, $8)",
        )
        .bind(url)
        .bind(title)
        .bind(topic)
        .bind(feed)
        .bind(story)
        .bind(score)
        .bind(state)
        .bind(now)
        .execute(pool)
        .await
        .expect("insert article");
    }
}

fn admin() -> QueryContext {
    QueryContext {
        current_user_id: None,
        viewer: Some(UserContext::authenticated(
            Uuid::now_v7(),
            vec!["administer site".to_string()],
        )),
        url_args: HashMap::new(),
        language: None,
    }
}

async fn gather_with(gather: &GatherService, def: QueryDefinition) -> Vec<serde_json::Value> {
    gather
        .execute_definition_with_stages(
            &def,
            &QueryDisplay::default(),
            1,
            HashMap::new(),
            &[],
            &admin(),
        )
        .await
        .expect("gather execution")
        .items
}

#[tokio::test(flavor = "multi_thread")]
async fn record_gather_and_reverse_reference() {
    let pool = sqlx::postgres::PgPool::connect(&database_url())
        .await
        .expect("connect to test database");
    let story_a = Uuid::now_v7();
    let story_b = Uuid::now_v7();
    seed(&pool, story_a, story_b).await;
    let gather = wire(pool.clone()).await;

    // M1-11: the unfiltered gather over the record type returns every seeded row.
    let all = gather_with(
        &gather,
        QueryDefinition {
            record_type: Some(RECORD_TYPE.to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(all.len(), 3, "gather reflects all seeded records: {all:?}");
    // Field access is applied: the mapped fields are present.
    assert!(all.iter().all(|r| r.get("url").is_some()));
    assert!(all.iter().any(|r| r.get("relevance_score").is_some()));

    // M1-11: filter by pipeline_state.
    let discarded = gather_with(
        &gather,
        QueryDefinition {
            record_type: Some(RECORD_TYPE.to_string()),
            filters: vec![
                serde_json::from_value(serde_json::json!({
                    "field": "pipeline_state",
                    "operator": "equals",
                    "value": "discarded",
                    "exposed": false,
                    "exposed_label": null
                }))
                .unwrap(),
            ],
            ..Default::default()
        },
    )
    .await;
    assert_eq!(discarded.len(), 1, "one discarded row: {discarded:?}");

    // M1-2: reverse reference — resolve story_a to its articles via the story_id
    // record field. Two articles reference story_a; the third references story_b.
    let for_story_a = gather_with(
        &gather,
        QueryDefinition {
            record_type: Some(RECORD_TYPE.to_string()),
            filters: vec![
                serde_json::from_value(serde_json::json!({
                    "field": "story_id",
                    "operator": "equals",
                    "value": story_a.to_string(),
                    "exposed": false,
                    "exposed_label": null
                }))
                .unwrap(),
            ],
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        for_story_a.len(),
        2,
        "story_a resolves to its two articles: {for_story_a:?}"
    );

    sqlx::query("DELETE FROM argus_articles")
        .execute(&pool)
        .await
        .expect("cleanup");
}
