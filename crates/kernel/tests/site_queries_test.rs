#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B80.5: a site plugin's six statements with no `raw_sql`.
//!
//! `plugins/test_site_queries` does the work trovato.rs held `raw_sql` for (a
//! listing, a sitemap, a docs index, one doc by its alias and a translation
//! sync) through `query` and `item-api`, and declares no `db`, `db_tables` or
//! `raw_sql`. These run the real plugin, built with the real SDK, through the
//! app's dispatcher, as the user a request would carry, and check what each
//! statement returned: the rows, their order, their URLs, and what the kernel
//! kept out (drafts, other stages) without the plugin asking.
//!
//! Requires Postgres, Redis, the translation plugin's table and the
//! `test_site_queries` fixture `.wasm` (built by CI before the test job).

mod common;

use common::{TestApp, run_test};
use serde_json::{Value, json};
use sqlx::PgPool;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateUrlAlias, UrlAlias};
use trovato_kernel::plugin::PluginInfo;
use trovato_kernel::tap::{RequestState, UserContext};
use trovato_sdk::host_errors;
use uuid::Uuid;

const PLUGIN: &str = "test_site_queries";

/// This file's own content types, so its rows never meet another file's.
const BLOG: &str = "b80s_blog";
const NEWS: &str = "b80s_news";
const DOCS: &str = "b80s_docs";
const FRONT: &str = "b80s_front";

// =============================================================================
// App: the shared shape, with the fixture loaded
// =============================================================================

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("site queries app init thread panicked")
    })
}

/// An app with the fixture loaded, which is then disabled again so no app
/// built later loads it.
async fn build_app() -> TestApp {
    trovato_test_utils::env::load_dotenv();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = PgPool::connect(&database_url).await.expect("connect");
    trovato_kernel::plugin::status::install_plugin(&pool, PLUGIN, "1.0.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{PLUGIN}': {e:#}"));
    let app = TestApp::with_config(|config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
    })
    .await;
    sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = $1")
        .bind(PLUGIN)
        .execute(&pool)
        .await
        .expect("disable the fixture");
    pool.close().await;
    for machine in [BLOG, NEWS, DOCS, FRONT] {
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, $1, 'site queries fixture', true, 'Title', 'core', '{}'::jsonb) \
             ON CONFLICT (type) DO NOTHING",
        )
        .bind(machine)
        .execute(&app.db)
        .await
        .expect("seed item type");
    }
    app
}

/// Run one op of the fixture as `user`, the way a request would.
async fn site(user: UserContext, op: Value) -> Value {
    let app = app();
    let state = RequestState::new(user, app.state.tap_services().clone());
    let result = app
        .state
        .tap_dispatcher()
        .dispatch_to_plugin("tap_queue_worker", &op.to_string(), PLUGIN, state)
        .await
        .unwrap_or_else(|| panic!("the fixture did not run {op}"));
    let out: Value = serde_json::from_str(&result.output).expect("the fixture answers JSON");
    // Shown with `--nocapture`: what each statement returned, for the record.
    println!("{op}\n  -> {out}");
    out
}

fn visitor() -> UserContext {
    let mut user = UserContext::anonymous();
    user.permissions = vec!["access content".to_string()];
    user
}

fn editor() -> UserContext {
    UserContext::authenticated(
        Uuid::now_v7(),
        vec![
            "access content".to_string(),
            "view any content".to_string(),
            "edit any content".to_string(),
        ],
    )
}

/// A marker for this test's items; the fixture narrows every op to it.
///
/// Hyphenated on purpose: a filter value is read as a `Uuid` when it parses as
/// one, and rendered hyphenated, so a simple-form marker would never equal the
/// text stored in `fields.run`.
fn new_run() -> String {
    Uuid::now_v7().to_string()
}

struct Seed<'a> {
    item_type: &'a str,
    title: String,
    status: i16,
    created: i64,
    changed: i64,
    stage: Uuid,
    fields: Value,
}

impl<'a> Seed<'a> {
    fn new(item_type: &'a str, run: &str, n: i64) -> Self {
        Self {
            item_type,
            title: format!("{item_type} {n}"),
            status: 1,
            created: 1_700_000_000 + n,
            changed: 1_700_000_000 + n,
            stage: LIVE_STAGE_ID,
            fields: json!({"run": run}),
        }
    }
}

/// Seed one item; it, its aliases and its translations go when the test ends.
async fn seed(app: &TestApp, s: Seed<'_>) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO item (id, type, title, status, author_id, created, changed, stage_id, fields) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(s.item_type)
    .bind(&s.title)
    .bind(s.status)
    .bind(Uuid::nil())
    .bind(s.created)
    .bind(s.changed)
    .bind(s.stage)
    .bind(&s.fields)
    .execute(&app.db)
    .await
    .expect("seed item");
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(format!("/item/{id}"))
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM item_translation WHERE item_id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
    });
    id
}

async fn alias(app: &TestApp, id: Uuid, path: &str) {
    UrlAlias::create(
        &app.db,
        CreateUrlAlias {
            source: format!("/item/{id}"),
            alias: path.to_string(),
            language: Some("en".to_string()),
            stage_id: Some(LIVE_STAGE_ID),
        },
    )
    .await
    .expect("create alias");
}

/// An internal stage, removed when the test ends.
async fn internal_stage(app: &TestApp) -> Uuid {
    let id = Uuid::now_v7();
    let suffix = &id.simple().to_string()[24..];
    sqlx::query(
        "INSERT INTO category_tag (id, category_id, label, weight, created, changed) \
         VALUES ($1, 'stages', $2, 0, 1, 1)",
    )
    .bind(id)
    .bind(format!("b80s {suffix}"))
    .execute(&app.db)
    .await
    .expect("insert stage tag");
    sqlx::query(
        "INSERT INTO stage_config (tag_id, machine_name, visibility, is_default) \
         VALUES ($1, $2, 'internal', false)",
    )
    .bind(id)
    .bind(format!("b80s_{suffix}"))
    .execute(&app.db)
    .await
    .expect("insert stage config");
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        sqlx::query("DELETE FROM item WHERE stage_id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM category_tag WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
    });
    id
}

fn ids(rows: &Value) -> Vec<String> {
    rows.as_array()
        .unwrap_or_else(|| panic!("rows: {rows}"))
        .iter()
        .map(|r| r["id"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn strs(ids: &[Uuid]) -> Vec<String> {
    ids.iter().map(Uuid::to_string).collect()
}

// =============================================================================
// The manifest
// =============================================================================

/// The fixture is the proof only while it holds none of the raw grants.
#[test]
fn the_fixture_declares_no_raw_sql_and_no_db() {
    let dir = common::project_root().join("plugins").join(PLUGIN);
    let info = PluginInfo::parse(&dir.join(format!("{PLUGIN}.info.toml"))).expect("manifest");
    let caps = info.capabilities.expect("capabilities declared");
    assert!(!caps.raw_sql, "the fixture must not hold raw_sql");
    assert!(caps.db_tables.is_empty(), "nor any db_tables");
    let mut interfaces = caps.host_interfaces.clone();
    interfaces.sort();
    assert_eq!(interfaces, vec!["item-api", "logging", "query"]);
}

// =============================================================================
// The six statements
// =============================================================================

/// `LISTING_SQL`: the newest blog and news posts, limited, each with its URL;
/// a draft, a doc and a post on another stage are not among them.
#[test]
fn the_listing_is_the_newest_posts_of_two_types_with_urls() {
    run_test(async {
        let app = app();
        let run = new_run();
        let stage = internal_stage(app).await;
        let oldest = seed(app, Seed::new(BLOG, &run, 1)).await;
        let middle = seed(app, Seed::new(NEWS, &run, 2)).await;
        let newest = seed(app, Seed::new(BLOG, &run, 3)).await;
        let _doc = seed(app, Seed::new(DOCS, &run, 4)).await;
        let _draft = seed(
            app,
            Seed {
                status: 0,
                ..Seed::new(NEWS, &run, 5)
            },
        )
        .await;
        let _staged = seed(
            app,
            Seed {
                stage,
                ..Seed::new(BLOG, &run, 6)
            },
        )
        .await;
        let path = format!("/blog/b80s-{newest}");
        alias(app, newest, &path).await;

        let op = |limit: u64| json!({"op": "listing", "types": [BLOG, NEWS], "limit": limit, "run": run});
        let out = site(editor(), op(2)).await;
        let posts = &out["posts"];
        assert_eq!(ids(posts), strs(&[newest, middle]), "{out}");
        assert_eq!(posts[0]["url"], json!(path));
        assert_eq!(posts[1]["url"], json!(format!("/item/{middle}")));
        assert_eq!(posts[1]["type"], json!(NEWS));
        assert_eq!(posts[0]["title"], json!(format!("{BLOG} 3")));

        let out = site(visitor(), op(10)).await;
        assert_eq!(
            ids(&out["posts"]),
            strs(&[newest, middle, oldest]),
            "every published post on the live stage, and nothing else: {out}"
        );
    });
}

/// `SITEMAP_SQL`: every published item but the front page type, most recently
/// changed first, across pages.
#[test]
fn the_sitemap_reads_every_page_most_recently_changed_first() {
    run_test(async {
        let app = app();
        let run = new_run();
        let mut expected = Vec::new();
        for n in 0..105 {
            let item_type = [BLOG, NEWS, DOCS][usize::try_from(n).unwrap() % 3];
            expected.push(
                seed(
                    app,
                    Seed {
                        changed: 1_800_000_000 + n,
                        ..Seed::new(item_type, &run, n)
                    },
                )
                .await,
            );
        }
        let _front = seed(app, Seed::new(FRONT, &run, 200)).await;
        let _draft = seed(
            app,
            Seed {
                status: 0,
                ..Seed::new(BLOG, &run, 201)
            },
        )
        .await;
        expected.reverse();

        let out = site(
            visitor(),
            json!({"op": "sitemap", "exclude": FRONT, "run": run}),
        )
        .await;
        assert_eq!(
            out["pages"], 2,
            "105 rows are two pages of 100: {}",
            out["pages"]
        );
        assert_eq!(ids(&out["entries"]), strs(&expected));
        assert!(
            out["entries"][0]["url"]
                .as_str()
                .unwrap()
                .starts_with("/item/"),
            "each entry carries its URL"
        );
    });
}

/// `DOCS_SQL`: the docs index by weight, unweighted last, ties by title; a doc
/// with no alias and a draft doc are left out.
#[test]
fn the_docs_index_sorts_by_weight_then_title_and_skips_unaliased_docs() {
    run_test(async {
        let app = app();
        let run = new_run();
        let weighted = |w: i64, title: &str, n: i64| Seed {
            title: title.to_string(),
            fields: json!({"run": run, "weight": w}),
            ..Seed::new(DOCS, &run, n)
        };
        let heavy = seed(app, weighted(10, "Zeta", 1)).await;
        let light_b = seed(app, weighted(-5, "Beta", 2)).await;
        let light_a = seed(app, weighted(-5, "Alpha", 3)).await;
        let unweighted = seed(
            app,
            Seed {
                title: "Aardvark".to_string(),
                ..Seed::new(DOCS, &run, 4)
            },
        )
        .await;
        let unaliased = seed(app, weighted(0, "Nowhere", 5)).await;
        let draft = seed(
            app,
            Seed {
                status: 0,
                ..weighted(0, "Draft", 6)
            },
        )
        .await;
        for (id, slug) in [
            (heavy, "zeta"),
            (light_b, "beta"),
            (light_a, "alpha"),
            (unweighted, "aardvark"),
            (draft, "draft"),
        ] {
            alias(app, id, &format!("/learn/b80s-{run}-{slug}")).await;
        }

        let out = site(visitor(), json!({"op": "docs", "type": DOCS, "run": run})).await;
        let docs = &out["docs"];
        assert_eq!(
            ids(docs),
            strs(&[light_a, light_b, heavy, unweighted]),
            "weight ascending, ties by title, unweighted last, no alias no entry: {out}"
        );
        assert_eq!(docs[0]["url"], json!(format!("/learn/b80s-{run}-alpha")));
        // `fields.x` projects as text (`->>`); the sort is on the JSONB value.
        assert_eq!(docs[0]["weight"], json!("-5"));
        assert_eq!(docs[3]["weight"], Value::Null);
        let _ = unaliased;
    });
}

/// `DOC_MARKDOWN_SQL`: one doc's markdown by the address it is served at; a
/// draft at an alias and an alias that names nothing read as nothing.
#[test]
fn a_doc_is_found_by_its_alias() {
    run_test(async {
        let app = app();
        let run = new_run();
        let with = |markdown: &str, n: i64| Seed {
            fields: json!({"run": run, "markdown": markdown}),
            ..Seed::new(DOCS, &run, n)
        };
        let doc = seed(app, with("# Getting started", 1)).await;
        let draft = seed(
            app,
            Seed {
                status: 0,
                ..with("# Not yet", 2)
            },
        )
        .await;
        alias(app, doc, &format!("/learn/b80s-{run}-start")).await;
        alias(app, draft, &format!("/learn/b80s-{run}-soon")).await;

        let doc_at =
            |slug: &str| json!({"op": "doc", "type": DOCS, "slug": format!("b80s-{run}-{slug}")});
        let out = site(visitor(), doc_at("start")).await;
        assert_eq!(out["markdown"], json!("# Getting started"), "{out}");
        let out = site(visitor(), doc_at("soon")).await;
        assert_eq!(
            out["markdown"],
            Value::Null,
            "a draft reads as nothing: {out}"
        );
        let out = site(visitor(), doc_at("missing")).await;
        assert_eq!(out["markdown"], Value::Null, "{out}");
    });
}

/// The translation statements: the sync writes what is declared and missing
/// or different, leaves what already matches, and removes what is no longer
/// declared; all of it as `ItemService` writes, so a visitor's sync is refused.
#[test]
fn the_translation_sync_writes_leaves_and_removes() {
    run_test(async {
        let app = app();
        common::ensure_translation_table(app);
        let run = new_run();
        let first = seed(app, Seed::new(DOCS, &run, 1)).await;
        let second = seed(app, Seed::new(DOCS, &run, 2)).await;
        let declared = json!({
            first.to_string(): {
                "it": {"title": "Primo", "fields": {"summary": "uno"}},
                "he": {"title": "Rishon", "fields": {}}
            },
            second.to_string(): {"it": {"title": "Secondo", "fields": {}}}
        });
        let sync = |declared: &Value| json!({"op": "translations", "type": DOCS, "run": run, "declared": declared});

        let out = site(editor(), sync(&declared)).await;
        assert_eq!(out["items"], 2, "{out}");
        assert_eq!(
            (
                out["written"].as_u64(),
                out["unchanged"].as_u64(),
                out["removed"].as_u64()
            ),
            (Some(3), Some(0), Some(0)),
            "{out}"
        );
        assert_eq!(out["errors"], json!([]));
        assert_eq!(
            stored(app, first).await,
            vec![
                ("he".into(), "Rishon".into()),
                ("it".into(), "Primo".into())
            ]
        );
        let changed_before = changed_of(app, first, "it").await;

        let out = site(editor(), sync(&declared)).await;
        assert_eq!(
            (
                out["written"].as_u64(),
                out["unchanged"].as_u64(),
                out["removed"].as_u64()
            ),
            (Some(0), Some(3), Some(0)),
            "{out}"
        );
        assert_eq!(
            changed_of(app, first, "it").await,
            changed_before,
            "an unchanged translation is not written"
        );

        let mut fewer = declared.clone();
        fewer[first.to_string()]
            .as_object_mut()
            .unwrap()
            .remove("he");
        fewer[second.to_string()]["it"]["title"] = json!("Secondo bis");
        let out = site(editor(), sync(&fewer)).await;
        assert_eq!(
            (
                out["written"].as_u64(),
                out["unchanged"].as_u64(),
                out["removed"].as_u64()
            ),
            (Some(1), Some(1), Some(1)),
            "{out}"
        );
        assert_eq!(
            stored(app, first).await,
            vec![("it".into(), "Primo".into())]
        );
        assert_eq!(
            stored(app, second).await,
            vec![("it".into(), "Secondo bis".into())]
        );

        let out = site(visitor(), sync(&json!({}))).await;
        assert_eq!(out["removed"], 0, "a visitor removes nothing: {out}");
        let codes: Vec<i64> = out["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["code"].as_i64().unwrap())
            .collect();
        assert!(
            !codes.is_empty()
                && codes
                    .iter()
                    .all(|&c| c == i64::from(host_errors::ERR_ITEM_ACCESS_DENIED)),
            "each delete a visitor tries is refused as an edit would be: {out}"
        );
        assert_eq!(stored(app, first).await.len(), 1, "and nothing went");
    });
}

async fn stored(app: &TestApp, item: Uuid) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT language, title FROM item_translation WHERE item_id = $1 ORDER BY language",
    )
    .bind(item)
    .fetch_all(&app.db)
    .await
    .expect("read item_translation")
}

async fn changed_of(app: &TestApp, item: Uuid, language: &str) -> i64 {
    sqlx::query_scalar("SELECT changed FROM item_translation WHERE item_id = $1 AND language = $2")
        .bind(item)
        .bind(language)
        .fetch_one(&app.db)
        .await
        .expect("read changed")
}

// =============================================================================
// `raw_sql` is visible
// =============================================================================

/// Boot names every loaded plugin that holds `raw_sql`, and only those:
/// `test_item_writer` holds it, this fixture does not.
#[test]
fn boot_names_the_plugins_holding_raw_sql() {
    use trovato_kernel::plugin::{PluginConfig, PluginRuntime};

    let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("create runtime");
    for name in ["test_item_writer", PLUGIN] {
        runtime
            .load_plugin(&common::project_root().join("plugins").join(name))
            .unwrap_or_else(|e| panic!("load fixture '{name}': {e:#}"));
    }
    assert_eq!(runtime.raw_sql_holders(), vec!["test_item_writer"]);
}

/// The admin plugin listing marks each plugin whose manifest grants `raw_sql`
/// and says what the grant means; a plugin without it carries no mark.
#[test]
fn the_admin_plugin_listing_marks_raw_sql() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;

    run_test(async {
        let app = app();
        let name = format!("b80s_admin_{}", &Uuid::now_v7().simple().to_string()[20..]);
        let cookies = app
            .create_and_login_admin(&name, "password123", &format!("{name}@test.com"))
            .await;
        let response = app
            .request_with_cookies(
                Request::get("/admin/plugins").body(Body::empty()).unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8_lossy(&body);

        let row = |plugin: &str| {
            html.split("<tr>")
                .find(|row| row.contains(&format!(r#"<code id="row-{plugin}">{plugin}</code>"#)))
                .unwrap_or_else(|| panic!("no row for {plugin}"))
                .to_string()
        };
        assert!(
            row("test_item_writer").contains("data-raw-sql=\"test_item_writer\""),
            "a raw_sql holder is marked"
        );
        assert!(
            !row(PLUGIN).contains("data-raw-sql"),
            "a plugin without raw_sql is not"
        );
        assert!(
            html.contains("id=\"plugin-raw-sql-note\""),
            "and the listing says what the grant means"
        );
    });
}
