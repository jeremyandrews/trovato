#![allow(clippy::unwrap_used, clippy::expect_used)]
//! P11g / D-59 freeze-gating fixture — the lightweight-record tier exercised
//! end-to-end through a **real** `GatherService` over a **real** table and the
//! **real** FR-8 `tap_field_access` seam.
//!
//! Drives `plugins/trovato_record_ref` (built from Rust with the real SDK and
//! the real `wasm32-wasip1` toolchain): it declares the `event_record`
//! lightweight-record type and governs its `secret_notes` field with
//! `tap_field_access`. This test proves the **non-negotiable D-55 fence** — a
//! plugin's `tap_field_access` governs a lightweight record exactly as it governs
//! an Item, through the one seam, deny-wins, fail-open — and the D-54 record-level
//! (published) visibility, without ever freezing an unexercised payload.
//!
//! Requires Postgres (the gather runs real SQL over `record_event`); CI builds
//! the fixture `.wasm` before the test job. Locally:
//!   cargo build -p trovato_record_ref --target wasm32-wasip1 --release \
//!     && cp target/wasm32-wasip1/release/trovato_record_ref.wasm plugins/trovato_record_ref/

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

const PLUGIN: &str = "trovato_record_ref";
const RECORD_TYPE: &str = "event_record";

/// Repo `plugins/` directory (two levels up from this crate).
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

/// Wire a standalone `GatherService` whose dispatcher has the reference plugin
/// loaded and whose registry carries its `event_record` declaration — the same
/// objects `AppState` wires, assembled directly so the test owns the DB rows.
async fn wire(pool: sqlx::PgPool) -> Arc<GatherService> {
    let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
    runtime
        .load_plugin(&plugins_dir().join(PLUGIN))
        .unwrap_or_else(|e| {
            panic!(
                "failed to load fixture '{PLUGIN}': {e:#}\n\
                 build it first: cargo build -p {PLUGIN} --target wasm32-wasip1 --release \
                 && cp target/wasm32-wasip1/release/{PLUGIN}.wasm plugins/{PLUGIN}/"
            )
        });
    let runtime = Arc::new(runtime);

    // Registry from the loaded plugin's declaration — validated against its
    // effective DB allowlist (record_event is migration-owned) and admitted.
    let compiled = runtime.get_plugin(PLUGIN).expect("plugin loaded");
    let (registry, errors) = RecordTypeRegistry::build(
        [(
            PLUGIN,
            compiled.db_policy().as_ref(),
            compiled.info.record_types.as_slice(),
        )],
        &HashSet::new(),
    );
    assert!(errors.is_empty(), "record type rejected: {errors:?}");
    assert!(registry.contains(RECORD_TYPE), "record type not registered");

    let registry_arc = Arc::new(registry);
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
    gather.set_record_types(registry_arc);
    gather
}

/// Run the fixture's migration (create `record_event`) and seed one published
/// and one unpublished event, each carrying a governed `secret_notes` field.
async fn seed(pool: &sqlx::PgPool, published: Uuid, unpublished: Uuid) {
    let migration = std::fs::read_to_string(
        plugins_dir().join(format!("{PLUGIN}/migrations/001_create_event_record.sql")),
    )
    .expect("read migration");
    sqlx::query(&migration)
        .execute(pool)
        .await
        .expect("create record_event");
    // Isolate from any prior run.
    sqlx::query("TRUNCATE record_event")
        .execute(pool)
        .await
        .expect("truncate");

    let now = 1_700_000_000_i64;
    for (id, title, is_pub) in [
        (published, "Public Event", true),
        (unpublished, "Draft Event", false),
    ] {
        sqlx::query(
            "INSERT INTO record_event \
             (id, title, author_id, published, location, capacity, secret_notes, created, changed) \
             VALUES ($1, $2, NULL, $3, 'Barga', 200, 'classified', $4, $4)",
        )
        .bind(id)
        .bind(title)
        .bind(is_pub)
        .bind(now)
        .execute(pool)
        .await
        .expect("insert event");
    }
}

fn context(viewer: UserContext) -> QueryContext {
    QueryContext {
        current_user_id: None,
        viewer: Some(viewer),
        url_args: HashMap::new(),
        language: None,
    }
}

async fn gather_rows(gather: &GatherService, viewer: UserContext) -> Vec<serde_json::Value> {
    let def = QueryDefinition {
        record_type: Some(RECORD_TYPE.to_string()),
        ..Default::default()
    };
    gather
        .execute_definition_with_stages(
            &def,
            &QueryDisplay::default(),
            1,
            HashMap::new(),
            &[],
            &context(viewer),
        )
        .await
        .expect("gather execution")
        .items
}

#[tokio::test(flavor = "multi_thread")]
async fn lightweight_record_gather_enforces_published_and_field_access() {
    let pool = sqlx::postgres::PgPool::connect(&database_url())
        .await
        .expect("connect to test database");
    let published = Uuid::now_v7();
    let unpublished = Uuid::now_v7();
    seed(&pool, published, unpublished).await;
    let gather = wire(pool.clone()).await;

    // 1) Anonymous viewer: sees only the PUBLISHED row (D-54 record-level
    //    visibility), and its governed `secret_notes` field is DENIED (D-55),
    //    while the ungoverned `location` stays visible (fail-open).
    let anon = gather_rows(&gather, UserContext::anonymous()).await;
    assert_eq!(anon.len(), 1, "anon sees only published rows: {anon:?}");
    let row = &anon[0];
    assert_eq!(
        row.get("id").and_then(|v| v.as_str()),
        Some(published.to_string()).as_deref()
    );
    assert!(
        row.get("secret_notes").is_none(),
        "governed field must be denied to a viewer without 'view secret_notes': {row}"
    );
    assert_eq!(
        row.get("location").and_then(|v| v.as_str()),
        Some("Barga"),
        "ungoverned field stays visible (fail-open)"
    );

    // 2) Permitted viewer (holds "view secret_notes"): still published-only, but
    //    now the governed field is ALLOWED through the SAME seam.
    let permitted =
        UserContext::authenticated(Uuid::now_v7(), vec!["view secret_notes".to_string()]);
    let permitted_rows = gather_rows(&gather, permitted).await;
    assert_eq!(
        permitted_rows.len(),
        1,
        "still published-only for a non-admin"
    );
    assert_eq!(
        permitted_rows[0]
            .get("secret_notes")
            .and_then(|v| v.as_str()),
        Some("classified"),
        "permitted viewer sees the governed field"
    );

    // 3) Admin: sees the unpublished row too (record-level admin bypass) and every
    //    field (field-access admin bypass) — no dispatch, all visible.
    let admin = UserContext::authenticated(Uuid::now_v7(), vec!["administer site".to_string()]);
    let admin_rows = gather_rows(&gather, admin).await;
    assert_eq!(admin_rows.len(), 2, "admin sees published + unpublished");
    for row in &admin_rows {
        assert!(
            row.get("secret_notes").is_some(),
            "admin bypass shows every field: {row}"
        );
    }

    // Cleanup.
    sqlx::query("DELETE FROM record_event WHERE id = ANY($1)")
        .bind(vec![published, unpublished])
        .execute(&pool)
        .await
        .expect("cleanup");
}
