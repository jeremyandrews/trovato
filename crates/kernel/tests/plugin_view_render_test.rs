#![allow(clippy::unwrap_used, clippy::expect_used)]
//! What a plugin's view tap actually puts on the page (**K1 fix 3**,
//! G-VIEW-OUTPUT-JSON-ENCODED).
//!
//! `#[plugin_tap]` serializes a tap's return with `serde_json::to_string`, so a
//! `String`-returning `tap_item_view` crosses the wire as a JSON string literal.
//! The item route used to append that literal to the page verbatim: a stray `"`
//! at each end, and a backslash inside every double-quoted attribute. No plugin
//! could render correct markup.
//!
//! This drives the **real** `plugins/trovato_series` wasm — the plugin the
//! friction log named as living proof — through the real `TapDispatcher` and a
//! real Postgres, and asserts on the markup `ItemService::load_for_view` hands
//! the route.
//!
//! Build the wasm first:
//!
//! ```text
//! cargo build -p trovato_series --target wasm32-wasip1 --release \
//!   && cp target/wasm32-wasip1/release/trovato_series.wasm plugins/trovato_series/
//! ```

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Duration;

use sqlx::PgPool;
use uuid::Uuid;

use trovato_kernel::content::{ContentTypeRegistry, ItemService, decode_view_output};
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::tap::{RequestServices, TapDispatcher, TapRegistry, UserContext};

const PLUGIN: &str = "trovato_series";
const LIVE_STAGE: &str = "0193a5a0-0000-7000-8000-000000000001";
const SERIES: &str = "Rust in \"anger\" & <production>";

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
    ContentTypeRegistry::new(pool.clone(), Duration::from_secs(60))
        .sync_from_plugins(&dispatcher())
        .await
        .expect("sync content types");
    pool
}

fn item_service(pool: &PgPool) -> Arc<ItemService> {
    let disp = dispatcher();
    let services =
        RequestServices::for_background(pool.clone(), None, None, reqwest::Client::new())
            .with_plugin_runtime(disp.runtime().clone());
    Arc::new(ItemService::new(
        pool.clone(),
        disp,
        services,
        Duration::from_secs(60),
        None,
        None,
    ))
}

async fn any_user(pool: &PgPool) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users ORDER BY created LIMIT 1")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Seed a published blog post in the series, `created` ordering the sequence.
async fn seed_post(pool: &PgPool, title: &str, created: i64) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO item (id, type, title, fields, status, author_id, stage_id, \
                           created, changed, language) \
         VALUES ($1, 'blog', $2, $3, 1, $4, $5, $6, $6, 'en')",
    )
    .bind(id)
    .bind(title)
    .bind(serde_json::json!({"field_series_title": SERIES}))
    .bind(any_user(pool).await)
    .bind(Uuid::parse_str(LIVE_STAGE).unwrap())
    .bind(created)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn reset(pool: &PgPool) {
    sqlx::query("DELETE FROM item WHERE type = 'blog'")
        .execute(pool)
        .await
        .unwrap();
}

/// The finding, closed: a blog post in a series renders navigation **markup**,
/// not a quoted, backslash-riddled JSON literal.
#[test]
fn a_series_post_renders_navigation_markup_on_the_page() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let base = chrono::Utc::now().timestamp();
        let first = seed_post(&pool, "Part one", base).await;
        let middle = seed_post(&pool, "Part <two> & \"more\"", base + 1).await;
        let last = seed_post(&pool, "Part three", base + 2).await;

        let items = item_service(&pool);
        let user = UserContext::authenticated(any_user(&pool).await, vec!["access content".into()]);

        let (_item, outputs) = items
            .load_for_view(middle, &user)
            .await
            .expect("load_for_view")
            .expect("post is viewable");
        let html = outputs.join("");

        // The defect, gone: no envelope, and no escaped quote anywhere. Before
        // the fix this fragment arrived as `"<nav class=\"series-nav\"...` and
        // the browser rendered a literal quote and a backslashed attribute.
        assert!(
            !html.starts_with('"') && !html.ends_with('"'),
            "the JSON envelope reached the page: {html}"
        );
        assert!(
            !html.contains("\\\""),
            "a serde-escaped quote reached the page: {html}"
        );

        // It is real markup, with real attributes.
        assert!(
            html.contains(r#"<nav class="series-nav""#),
            "expected series navigation markup, got: {html}"
        );
        assert!(html.contains("Part 2 of 3"), "position, got: {html}");
        assert!(
            html.contains(&format!(r#"<a rel="prev" href="/item/{first}">"#)),
            "previous link, got: {html}"
        );
        assert!(
            html.contains(&format!(r#"<a rel="next" href="/item/{last}">"#)),
            "next link, got: {html}"
        );

        // And the plugin's own escaping survives the round trip: the series
        // title carries `"`, `&` and `<`, which must arrive as entities rather
        // than as characters that close the attribute or open a tag.
        assert!(
            html.contains("Rust in &quot;anger&quot; &amp; &lt;production&gt;"),
            "the series title must arrive HTML-escaped, got: {html}"
        );
        assert!(
            !html.contains("<production>"),
            "an unescaped angle bracket reached the page: {html}"
        );
    });
}

/// The ends of the series carry only the link that exists.
#[test]
fn the_first_and_last_posts_each_carry_one_link() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let base = chrono::Utc::now().timestamp();
        let first = seed_post(&pool, "Opener", base).await;
        let last = seed_post(&pool, "Closer", base + 1).await;

        let items = item_service(&pool);
        let user = UserContext::authenticated(any_user(&pool).await, vec!["access content".into()]);

        let render = |id: Uuid| {
            let items = items.clone();
            let user = user.clone();
            async move {
                items
                    .load_for_view(id, &user)
                    .await
                    .expect("load_for_view")
                    .expect("viewable")
                    .1
                    .join("")
            }
        };

        let opener = render(first).await;
        assert!(opener.contains("Part 1 of 2"), "got: {opener}");
        assert!(!opener.contains(r#"rel="prev""#), "got: {opener}");
        assert!(opener.contains(r#"rel="next""#), "got: {opener}");

        let closer = render(last).await;
        assert!(closer.contains("Part 2 of 2"), "got: {closer}");
        assert!(closer.contains(r#"rel="prev""#), "got: {closer}");
        assert!(!closer.contains(r#"rel="next""#), "got: {closer}");
    });
}

/// A post that is not in a series contributes nothing — and specifically not
/// an empty-string fragment wearing a JSON envelope (`"\"\""`), which is what
/// the undecoded path used to append for every non-participating plugin.
#[test]
fn a_post_outside_a_series_contributes_no_fragment() {
    serial(async {
        let pool = fresh_pool().await;
        reset(&pool).await;

        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO item (id, type, title, fields, status, author_id, stage_id, \
                               created, changed, language) \
             VALUES ($1, 'blog', 'Standalone', '{}'::jsonb, 1, $2, $3, $4, $4, 'en')",
        )
        .bind(id)
        .bind(any_user(&pool).await)
        .bind(Uuid::parse_str(LIVE_STAGE).unwrap())
        .bind(chrono::Utc::now().timestamp())
        .execute(&pool)
        .await
        .unwrap();

        let items = item_service(&pool);
        let user = UserContext::authenticated(any_user(&pool).await, vec!["access content".into()]);
        let (_item, outputs) = items
            .load_for_view(id, &user)
            .await
            .expect("load_for_view")
            .expect("viewable");

        assert!(
            outputs.is_empty(),
            "an empty tap return must add nothing to the page, got: {outputs:?}"
        );
    });
}

/// The decoder's contract, at the unit level, against the exact wire form the
/// tap macro produces.
#[test]
fn the_decoder_unwraps_exactly_what_the_tap_macro_wraps() {
    let fragment = r#"<nav class="series-nav"><a href="/item/x">Prev &amp; back</a></nav>"#;
    let wire = serde_json::to_string(fragment).unwrap();
    assert_ne!(wire, fragment, "the wire form is the envelope");
    assert_eq!(decode_view_output(&wire), fragment);
}
