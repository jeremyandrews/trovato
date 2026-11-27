#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Precise timers from plugins (B278): `tap_timer`, the `timer` host
//! interface and its capability, and the first real consumer, scheduled
//! publishing.
//!
//! Build the fixtures first (`scripts/ci-build-plugins.sh` does both):
//! `cargo build -p test_timer -p trovato_scheduled_publishing --target wasm32-wasip1 --release`
//! and copy each `.wasm` into its `plugins/<name>/`.
//!
//! The tests here share the one `test_timer` owner, so they run one at a time
//! (a mutex under libtest; under nextest each is its own process, and each
//! uses timer names of its own).

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use trovato_kernel::cron::CronService;
use trovato_kernel::models::SiteConfig;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, UpdateItem};
use trovato_kernel::plugin::{PluginConfig, PluginRuntime};
use trovato_kernel::tap::{TapDispatcher, TapRegistry, UserContext};
use trovato_kernel::timer::{self, TimerRequest, TimerService, TimerSettings};

const FIXTURE: &str = "test_timer";

static SERIAL: Mutex<()> = Mutex::new(());

fn serial<F: std::future::Future<Output = ()> + Send>(body: F) {
    let _guard = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
    common::run_test(body);
}

fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

/// One dispatcher with the fixture loaded, for the whole binary.
fn dispatcher() -> Arc<TapDispatcher> {
    static DISPATCHER: OnceLock<Arc<TapDispatcher>> = OnceLock::new();
    DISPATCHER
        .get_or_init(|| {
            let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("runtime");
            runtime
                .load_plugin(&plugins_dir().join(FIXTURE))
                .unwrap_or_else(|e| {
                    panic!(
                        "load {FIXTURE}: {e:#}\nbuild it: cargo build -p {FIXTURE} \
                         --target wasm32-wasip1 --release && cp \
                         target/wasm32-wasip1/release/{FIXTURE}.wasm plugins/{FIXTURE}/"
                    )
                });
            let runtime = Arc::new(runtime);
            let registry = Arc::new(TapRegistry::from_plugins(&runtime));
            Arc::new(TapDispatcher::new(runtime, registry))
        })
        .clone()
}

/// A timer service that fires the fixture's timers through the real
/// dispatcher, scoped to the fixture so no other test's timers are claimed.
fn fixture_service(pool: &PgPool) -> Arc<TimerService> {
    let redis = redis::Client::open(trovato_test_utils::env::redis_url()).expect("redis");
    let mut cron = CronService::new(redis, pool.clone());
    cron.set_tap_dispatcher(dispatcher());
    let timers = Arc::new(TimerService::new(pool.clone(), Some(Arc::new(cron))));
    timers.set_owner_scope(Some(vec![FIXTURE.to_string()]));
    timers
}

/// A unique timer name, removed (with what the fixture reported for it) when
/// the test ends.
fn name(pool: &PgPool, stem: &str) -> String {
    let name = format!("{stem}-{}", &Uuid::new_v4().simple().to_string()[..10]);
    let (pool, n) = (pool.clone(), name.clone());
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM scheduled_timer WHERE owner = $1 AND name LIKE $2")
            .bind(FIXTURE)
            .bind(format!("{n}%"))
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM site_config WHERE key LIKE $1")
            .bind(format!("plugin.{FIXTURE}.fired.{n}%"))
            .execute(&pool)
            .await;
    });
    name
}

/// What `tap_timer` reported for `name`, once it has.
async fn report(pool: &PgPool, name: &str, limit: Duration) -> Option<Value> {
    let until = tokio::time::Instant::now() + limit;
    loop {
        if let Ok(Some(stored)) =
            SiteConfig::get(pool, &format!("plugin.{FIXTURE}.fired.{name}")).await
        {
            let value = match stored {
                Value::String(text) => serde_json::from_str(&text).ok(),
                other => Some(other),
            };
            if value.is_some() {
                return value;
            }
        }
        if tokio::time::Instant::now() > until {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn start(timers: &Arc<TimerService>) -> (CancellationToken, tokio::task::JoinHandle<()>) {
    let token = CancellationToken::new();
    let handle = tokio::spawn(timers.clone().run_scheduler(token.clone()));
    (token, handle)
}

async fn stop(token: CancellationToken, handle: tokio::task::JoinHandle<()>) {
    token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(30), handle).await;
}

/// A plugin's timer dispatches `tap_timer` to that plugin, with its name and
/// payload, at its second; from there the plugin sets its own timers, and
/// sees and cancels no one else's.
#[test]
fn a_plugin_timer_reaches_its_plugin_which_sees_only_its_own_timers() {
    serial(async {
        let app = common::shared_app().await;
        let pool = app.db.clone();
        let timers = fixture_service(&pool);

        // Another owner's timer, with a name the fixture will ask for.
        let other_owner = format!("other_{}", &Uuid::new_v4().simple().to_string()[..8]);
        let foreign = name(&pool, "foreign");
        timer::set(
            &pool,
            &other_owner,
            &TimerRequest {
                name: foreign.clone(),
                run_at: Utc::now().timestamp() + 3600,
                repeat_secs: None,
                payload: json!({}),
            },
            &TimerSettings::default(),
        )
        .await
        .unwrap();
        let p = pool.clone();
        let o = other_owner.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM scheduled_timer WHERE owner = $1")
                .bind(o)
                .execute(&p)
                .await;
        });

        let probe = name(&pool, "probe");
        let child = format!("{probe}-child");
        let at = Utc::now().timestamp() + 2;
        timers
            .set(
                FIXTURE,
                &TimerRequest {
                    name: probe.clone(),
                    run_at: at,
                    repeat_secs: None,
                    payload: json!({
                        "probe": foreign,
                        "set": { "name": child, "run_at": at + 3600, "payload": { "from": "plugin" } },
                    }),
                },
            )
            .await
            .unwrap();
        let (token, handle) = start(&timers);
        let seen = report(&pool, &probe, Duration::from_secs(15)).await;
        stop(token, handle).await;

        let seen = seen.expect("tap_timer ran for the plugin's timer");
        assert_eq!(seen["name"], json!(probe));
        assert_eq!(seen["run_at"], json!(at), "the time it was due");
        let late = seen["fired_at"].as_i64().unwrap() - at;
        assert!(
            (0..=1).contains(&late),
            "fired at its second: {late} s late"
        );
        assert_eq!(seen["attempt"], json!(1));
        assert_eq!(
            seen["probe"],
            json!({ "seen": null, "cancelled": false }),
            "another owner's timer of the same name is invisible to the plugin"
        );
        assert!(
            timer::get(&pool, &other_owner, &foreign)
                .await
                .unwrap()
                .is_some(),
            "and still there"
        );
        assert_eq!(seen["set"], json!(0), "the plugin set its own timer");
        let mine = timer::get(&pool, FIXTURE, &child)
            .await
            .unwrap()
            .expect("set under its owner");
        assert_eq!(mine.payload, json!({ "from": "plugin" }));
        assert!(
            timer::get(&pool, &other_owner, &child)
                .await
                .unwrap()
                .is_none(),
            "never under another owner"
        );
    });
}

/// The throttles reach the plugin as error codes, and nothing is stored.
#[test]
fn a_plugin_is_told_why_a_timer_is_refused() {
    serial(async {
        let app = common::shared_app().await;
        let pool = app.db.clone();
        let timers = fixture_service(&pool);
        // Room for the request it carries; the plugin's own `set` meets the
        // stored throttles, the shipped defaults.
        timers.set_settings_override(Some(TimerSettings {
            max_payload_bytes: 65_536,
            ..TimerSettings::default()
        }));
        let probe = name(&pool, "refused");
        let at = Utc::now().timestamp();
        for (suffix, request, code) in [
            (
                "fast",
                json!({ "name": format!("{probe}-fast"), "run_at": at + 60, "repeat_secs": 5 }),
                trovato_sdk::host_errors::ERR_TIMER_REPEAT_TOO_SHORT,
            ),
            (
                "big",
                json!({ "name": format!("{probe}-big"), "run_at": at + 60, "payload": { "x": "y".repeat(5000) } }),
                trovato_sdk::host_errors::ERR_TIMER_PAYLOAD_TOO_LARGE,
            ),
            (
                "unnamed",
                json!({ "name": "", "run_at": at + 60 }),
                trovato_sdk::host_errors::ERR_TIMER_INVALID,
            ),
        ] {
            let fire = format!("{probe}-{suffix}-go");
            timers
                .set(
                    FIXTURE,
                    &TimerRequest {
                        name: fire.clone(),
                        run_at: at,
                        repeat_secs: None,
                        payload: json!({ "set": request }),
                    },
                )
                .await
                .unwrap();
            let (token, handle) = start(&timers);
            let seen = report(&pool, &fire, Duration::from_secs(15)).await;
            stop(token, handle).await;
            assert_eq!(
                seen.expect("tap_timer ran")["set"],
                json!(code),
                "{suffix}: refused with its own code"
            );
        }
        let stored: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM scheduled_timer WHERE owner = $1 AND name IN ($2, $3)",
        )
        .bind(FIXTURE)
        .bind(format!("{probe}-fast"))
        .bind(format!("{probe}-big"))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored, 0, "a refused timer is not stored");
    });
}

/// A plugin that reports an error is retried, then its timer is `failed`
/// with that error kept.
#[test]
fn a_failing_plugin_timer_retries_then_fails_with_its_error() {
    serial(async {
        let app = common::shared_app().await;
        let pool = app.db.clone();
        let timers = fixture_service(&pool);
        timers.set_retry_backoff(1, 1);
        let bad = name(&pool, "bad");
        timers
            .set(
                FIXTURE,
                &TimerRequest {
                    name: bad.clone(),
                    run_at: Utc::now().timestamp(),
                    repeat_secs: None,
                    payload: json!({ "fail": true }),
                },
            )
            .await
            .unwrap();
        let (token, handle) = start(&timers);
        let until = tokio::time::Instant::now() + Duration::from_secs(20);
        let mut last = None;
        while tokio::time::Instant::now() < until {
            last = timer::get(&pool, FIXTURE, &bad).await.unwrap();
            if last.as_ref().is_some_and(|t| t.status == "failed") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        stop(token, handle).await;
        let t = last.expect("kept");
        assert_eq!(t.status, "failed");
        assert_eq!(t.attempts, timer::MAX_ATTEMPTS);
        assert_eq!(t.last_error.as_deref(), Some("asked to fail on attempt 3"));
    });
}

/// A plugin that imports the `timer` interface without declaring it does not
/// load, and with it declared the interface resolves.
#[test]
fn a_plugin_without_the_timer_capability_is_refused() {
    let wat = r#"
    (module
      (import "trovato:kernel/timer" "set" (func (param i32 i32) (result i32)))
      (memory (export "memory") 1))
    "#;
    for (label, caps, loads) in [
        ("none", "", false),
        (
            "other",
            "\n[capabilities]\nhost_interfaces = [\"logging\"]\n",
            false,
        ),
        (
            "declared",
            "\n[capabilities]\nhost_interfaces = [\"timer\"]\n",
            true,
        ),
    ] {
        let mut runtime = PluginRuntime::new(&PluginConfig::default()).expect("runtime");
        let dir = std::env::temp_dir().join(format!(
            "trovato_b278_timer_cap_{label}_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("timerplug.info.toml"),
            format!("name = \"timerplug\"\ndescription = \"x\"\nversion = \"1.0.0\"\n{caps}"),
        )
        .unwrap();
        std::fs::write(dir.join("timerplug.wasm"), wat).unwrap();
        let result = runtime.load_plugin(&dir);
        std::fs::remove_dir_all(&dir).ok();
        if loads {
            result.unwrap_or_else(|e| panic!("[{label}] declared timer must load: {e:#}"));
        } else {
            let err = result
                .expect_err("an undeclared timer import must not load")
                .to_string();
            assert!(
                err.contains("imports host interface 'timer'") && err.contains("does not declare"),
                "[{label}] {err}"
            );
        }
    }
}

// =============================================================================
// Scheduled publishing
// =============================================================================

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

const PUBLISHER: &str = "trovato_scheduled_publishing";

/// The lock other suites take around enabling a fixture plugin and booting.
const FIXTURE_LOCK: i64 = 0x0040_0000_0001;

/// An app that loads scheduled publishing, alone, from a directory of its
/// own, enabled before boot so its taps are registered.
async fn publisher_app() -> common::TestApp {
    let source = plugins_dir().join(PUBLISHER);
    let dir = std::env::temp_dir().join(format!("trovato-b278-{}", Uuid::new_v4().simple()));
    let target = dir.join(PUBLISHER);
    std::fs::create_dir_all(target.join("migrations")).unwrap();
    for file in [
        format!("{PUBLISHER}.wasm"),
        format!("{PUBLISHER}.info.toml"),
    ] {
        std::fs::copy(source.join(&file), target.join(&file))
            .unwrap_or_else(|e| panic!("copy {file}: {e}; run scripts/ci-build-plugins.sh"));
    }
    for entry in std::fs::read_dir(source.join("migrations")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(
            entry.path(),
            target.join("migrations").join(entry.file_name()),
        )
        .unwrap();
    }
    trovato_test_utils::env::load_dotenv();
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    common::ensure_database_migrated(&url).await;
    let pool = PgPool::connect(&url).await.unwrap();
    // Enabled for this app's boot, then put back as it was, under the lock
    // `search_index_writers_test` and `background_item_api_test` take to
    // enable their fixtures, so no two boots see each other's change.
    let mut lock = pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(FIXTURE_LOCK)
        .execute(&mut *lock)
        .await
        .unwrap();
    let before: Option<i16> =
        sqlx::query_scalar("SELECT status FROM plugin_status WHERE name = $1")
            .bind(PUBLISHER)
            .fetch_optional(&pool)
            .await
            .unwrap();
    trovato_kernel::plugin::status::install_plugin(&pool, PUBLISHER, "1.0.0")
        .await
        .unwrap();
    let plugins = dir.clone();
    let app = common::TestApp::with_config(move |config| config.plugins_dirs = vec![plugins]).await;
    match before {
        Some(status) => sqlx::query("UPDATE plugin_status SET status = $1 WHERE name = $2")
            .bind(status)
            .bind(PUBLISHER)
            .execute(&pool)
            .await
            .map(|_| ()),
        None => sqlx::query("DELETE FROM plugin_status WHERE name = $1")
            .bind(PUBLISHER)
            .execute(&pool)
            .await
            .map(|_| ()),
    }
    .unwrap();
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(FIXTURE_LOCK)
        .execute(&mut *lock)
        .await
        .unwrap();
    drop(lock);
    pool.close().await;
    let plugins = dir.clone();
    let app = common::TestApp::with_config(move |config| config.plugins_dirs = vec![plugins]).await;
    std::fs::remove_dir_all(&dir).ok();
    app
}

async fn item_status(pool: &PgPool, id: Uuid) -> i16 {
    sqlx::query_scalar("SELECT status FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// An item saved with a future publish date goes live within a second of
/// it; changing the date moves the timer, clearing it cancels it, and
/// deleting the item cancels the rest.
#[test]
fn scheduled_publishing_lands_at_the_second() {
    serial(async {
        let app = publisher_app().await;
        let timers = app.state.timers().clone();
        timers.set_owner_scope(Some(vec![PUBLISHER.to_string()]));
        let pool = app.db.clone();
        let created = Arc::new(Mutex::new(Vec::<Uuid>::new()));
        {
            let (pool, created) = (pool.clone(), created.clone());
            common::defer_cleanup(move || async move {
                let ids = created.lock().unwrap().clone();
                for id in ids {
                    let _ = sqlx::query(
                        "DELETE FROM scheduled_timer WHERE owner = $1 AND name LIKE $2",
                    )
                    .bind(PUBLISHER)
                    .bind(format!("%:{id}"))
                    .execute(&pool)
                    .await;
                    let _ = sqlx::query("DELETE FROM item WHERE id = $1")
                        .bind(id)
                        .execute(&pool)
                        .await;
                }
            });
        }
        let create = |title: &str, fields: Value| CreateItem {
            item_type: "page".to_string(),
            title: title.to_string(),
            author_id: Uuid::nil(),
            status: Some(0),
            promote: Some(0),
            sticky: Some(0),
            fields: Some(fields),
            stage_id: Some(LIVE_STAGE_ID),
            language: Some("en".to_string()),
            log: Some("B278 fixture".to_string()),
        };

        // Saved with a publish date two seconds out.
        let publish_at = Utc::now().timestamp() + 2;
        let item = app
            .state
            .items()
            .create(
                create(
                    "Goes live at the second",
                    json!({ "field_publish_on": publish_at.to_string() }),
                ),
                &admin(),
            )
            .await
            .expect("create");
        created.lock().unwrap().push(item.id);
        let timer = timer::get(&pool, PUBLISHER, &format!("publish:{}", item.id))
            .await
            .unwrap()
            .expect("the save set a timer for the date");
        assert_eq!(timer.run_at.timestamp(), publish_at);

        let (token, handle) = start(&timers);
        let until = tokio::time::Instant::now() + Duration::from_secs(15);
        while item_status(&pool, item.id).await == 0 && tokio::time::Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let live_at = Utc::now();
        stop(token, handle).await;
        assert_eq!(
            item_status(&pool, item.id).await,
            1,
            "published by its timer"
        );
        let late_ms = live_at.timestamp_millis() - publish_at * 1000;
        eprintln!("published {late_ms} ms after its publish date");
        assert!(
            (0..1000).contains(&late_ms),
            "within a second: {late_ms} ms"
        );
        let log: Option<String> = sqlx::query_scalar(
            "SELECT log FROM item_revision WHERE item_id = $1 ORDER BY created DESC, id DESC LIMIT 1",
        )
        .bind(item.id)
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert_eq!(
            log.as_deref(),
            Some("Scheduled publish"),
            "an item API save, with a revision"
        );
        assert!(
            timer::get(&pool, PUBLISHER, &format!("publish:{}", item.id))
                .await
                .unwrap()
                .is_none(),
            "a one-shot timer is gone once it has fired"
        );

        // A second item: the date moves, then is cleared, then the item goes.
        let later = Utc::now().timestamp() + 3600;
        let other = app
            .state
            .items()
            .create(
                create("Moved, then cleared", json!({ "field_publish_on": later })),
                &admin(),
            )
            .await
            .unwrap();
        created.lock().unwrap().push(other.id);
        let name = format!("publish:{}", other.id);
        assert_eq!(
            timer::get(&pool, PUBLISHER, &name)
                .await
                .unwrap()
                .unwrap()
                .run_at
                .timestamp(),
            later
        );
        let update = |fields: Value| -> UpdateItem {
            serde_json::from_value(json!({ "fields": fields })).unwrap()
        };
        app.state
            .items()
            .update(
                other.id,
                update(json!({ "field_publish_on": later + 60 })),
                &admin(),
            )
            .await
            .unwrap();
        assert_eq!(
            timer::get(&pool, PUBLISHER, &name)
                .await
                .unwrap()
                .unwrap()
                .run_at
                .timestamp(),
            later + 60,
            "a changed date moves the timer"
        );
        app.state
            .items()
            .update(
                other.id,
                update(json!({ "field_unpublish_on": later + 120 })),
                &admin(),
            )
            .await
            .unwrap();
        assert!(
            timer::get(&pool, PUBLISHER, &name).await.unwrap().is_none(),
            "a cleared date cancels it"
        );
        app.state
            .items()
            .update(
                other.id,
                update(json!({ "field_publish_on": later })),
                &admin(),
            )
            .await
            .unwrap();
        assert!(timer::get(&pool, PUBLISHER, &name).await.unwrap().is_some());
        app.state.items().delete(other.id, &admin()).await.unwrap();
        assert!(
            timer::get(&pool, PUBLISHER, &name).await.unwrap().is_none(),
            "deleting the item cancels its timers"
        );
    });
}
