#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Precise timers beside cron (B278): the scheduler loop against the real
//! Postgres.
//!
//! Every test builds its own `TimerService` instances over the shared
//! database, scoped to an owner made for that test (`set_owner_scope`), so a
//! scheduler here never claims another test's timers, and handlers are Rust
//! closures registered for that owner. The plugin side (`tap_timer`, the
//! `timer` host interface and its capability) is in `timer_plugin_test.rs`.

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use trovato_kernel::timer::{
    FiredTimer, KernelHandler, MAX_ATTEMPTS, TimerRefused, TimerRequest, TimerService,
    TimerSettings,
};

/// An owner no other test uses.
fn owner() -> String {
    format!("t_{}", &Uuid::new_v4().simple().to_string()[..12])
}

/// Remove the owner's timers when the test ends, pass or fail.
fn cleanup(pool: &PgPool, owner: &str) {
    let pool = pool.clone();
    let owner = owner.to_string();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM scheduled_timer WHERE owner = $1")
            .bind(owner)
            .execute(&pool)
            .await;
    });
}

/// Test throttles: a one-second minimum repeat so cadence tests run in
/// seconds; the rest as shipped.
fn test_settings() -> TimerSettings {
    TimerSettings {
        min_repeat_secs: 1,
        ..TimerSettings::default()
    }
}

/// A service scoped to `owner`, with test throttles.
fn service(pool: &PgPool, owner: &str) -> Arc<TimerService> {
    let timers = Arc::new(TimerService::new(pool.clone(), None));
    timers.set_owner_scope(Some(vec![owner.to_string()]));
    timers.set_settings_override(Some(test_settings()));
    timers
}

/// Every fire a handler saw: name, when it was due, when it was claimed,
/// when the handler ran, and which service ran it.
#[derive(Debug, Clone)]
struct Fire {
    name: String,
    run_at: DateTime<Utc>,
    fired_at: DateTime<Utc>,
    seen_at: DateTime<Utc>,
    by: usize,
    attempt: i32,
}

type Log = Arc<Mutex<Vec<Fire>>>;

/// A handler that records each fire in `log` as service `by`.
fn recorder(log: &Log, by: usize) -> KernelHandler {
    let log = log.clone();
    Arc::new(move |t: FiredTimer| {
        let log = log.clone();
        Box::pin(async move {
            log.lock().unwrap().push(Fire {
                name: t.name,
                run_at: t.run_at,
                fired_at: t.fired_at,
                seen_at: Utc::now(),
                by,
                attempt: t.attempt,
            });
            Ok(())
        })
    })
}

/// Start the loop; returns its token and handle.
fn start(timers: &Arc<TimerService>) -> (CancellationToken, tokio::task::JoinHandle<()>) {
    let token = CancellationToken::new();
    let handle = tokio::spawn(timers.clone().run_scheduler(token.clone()));
    (token, handle)
}

async fn stop(token: CancellationToken, handle: tokio::task::JoinHandle<()>) {
    token.cancel();
    tokio::time::timeout(Duration::from_secs(30), handle)
        .await
        .expect("the scheduler stops")
        .expect("the scheduler did not panic");
}

/// The next whole second at least `secs` from now, as Unix seconds.
fn in_secs(secs: i64) -> i64 {
    Utc::now().timestamp() + secs
}

fn request(name: &str, run_at: i64) -> TimerRequest {
    TimerRequest {
        name: name.to_string(),
        run_at,
        repeat_secs: None,
        payload: serde_json::json!({}),
    }
}

/// Wait until `log` has at least `n` fires, or `limit` passes.
async fn wait_for(log: &Log, n: usize, limit: Duration) {
    let until = tokio::time::Instant::now() + limit;
    while log.lock().unwrap().len() < n && tokio::time::Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn status(pool: &PgPool, owner: &str, name: &str) -> Option<(String, i32, Option<String>)> {
    sqlx::query_as(
        "SELECT status, attempts, last_error FROM scheduled_timer WHERE owner = $1 AND name = $2",
    )
    .bind(owner)
    .bind(name)
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// A timer set for two seconds from now fires within a second of its time.
#[test]
fn a_timer_fires_within_a_second_of_its_time() {
    common::run_test(async {
        let app = common::shared_app().await;
        let owner = owner();
        cleanup(&app.db, &owner);
        let timers = service(&app.db, &owner);
        let log: Log = Arc::default();
        timers.register_handler(&owner, "soon", recorder(&log, 0));
        let (token, handle) = start(&timers);

        let at = in_secs(2);
        let set = timers.set(&owner, &request("soon", at)).await.expect("set");
        assert_eq!(set.run_at.timestamp(), at, "to the second");
        wait_for(&log, 1, Duration::from_secs(10)).await;
        stop(token, handle).await;

        let fires = log.lock().unwrap().clone();
        assert_eq!(fires.len(), 1, "fired once: {fires:?}");
        let late = (fires[0].seen_at - fires[0].run_at).num_milliseconds();
        assert!(
            (0..1000).contains(&late),
            "fired {late} ms after its time; due {}, ran {}",
            fires[0].run_at,
            fires[0].seen_at
        );
        assert!(
            status(&app.db, &owner, "soon").await.is_none(),
            "a one-shot timer is gone once it succeeds"
        );
    });
}

/// Two schedulers on one database: every timer fires exactly once.
#[test]
fn two_schedulers_fire_each_timer_exactly_once() {
    common::run_test(async {
        let app = common::shared_app().await;
        let owner = owner();
        cleanup(&app.db, &owner);
        let log: Log = Arc::default();
        let a = service(&app.db, &owner);
        let b = service(&app.db, &owner);
        // Room for every timer at once on either server, so both race for all.
        let wide = TimerSettings {
            max_concurrent: 50,
            ..test_settings()
        };
        a.set_settings_override(Some(wide));
        b.set_settings_override(Some(wide));
        a.register_handler(&owner, "t", recorder(&log, 0));
        b.register_handler(&owner, "t", recorder(&log, 1));
        let (ta, ha) = start(&a);
        let (tb, hb) = start(&b);

        // Thirty one-shot timers due in the same second, and one repeating
        // every second for four slots.
        let at = in_secs(2);
        for i in 0..30 {
            a.set(&owner, &request(&format!("t:{i}"), at))
                .await
                .unwrap();
        }
        let repeating = TimerRequest {
            repeat_secs: Some(1),
            ..request("t:every", at)
        };
        b.set(&owner, &repeating).await.unwrap();
        wait_for(&log, 34, Duration::from_secs(15)).await;
        // Room for a double fire to show up.
        tokio::time::sleep(Duration::from_millis(500)).await;
        timers_cancel(&a, &owner, "t:every").await;
        stop(ta, ha).await;
        stop(tb, hb).await;

        let fires = log.lock().unwrap().clone();
        let mut per_name: HashMap<String, usize> = HashMap::new();
        for f in fires.iter().filter(|f| f.name != "t:every") {
            *per_name.entry(f.name.clone()).or_default() += 1;
        }
        assert_eq!(
            per_name.len(),
            30,
            "every one-shot timer fired: {per_name:?}"
        );
        assert!(
            per_name.values().all(|n| *n == 1),
            "each exactly once: {per_name:?}"
        );
        // The repeating timer: one fire per slot, never two for the same one.
        let mut slots: HashMap<i64, usize> = HashMap::new();
        for f in fires.iter().filter(|f| f.name == "t:every") {
            *slots.entry(f.run_at.timestamp()).or_default() += 1;
        }
        assert!(
            slots.len() >= 3,
            "the repeating timer kept firing: {slots:?}"
        );
        assert!(
            slots.values().all(|n| *n == 1),
            "one fire per slot: {slots:?}"
        );
        let by: std::collections::HashSet<usize> = fires.iter().map(|f| f.by).collect();
        eprintln!("fired by servers {by:?}: {} fires", fires.len());
    });
}

async fn timers_cancel(timers: &TimerService, owner: &str, name: &str) {
    timers.cancel(owner, name).await.unwrap();
}

/// A loop asleep until a far timer wakes for an earlier one registered by
/// another server, through Postgres NOTIFY, not at its minute recheck.
#[test]
fn registering_an_earlier_timer_wakes_a_sleeping_loop() {
    common::run_test(async {
        let app = common::shared_app().await;
        let owner = owner();
        cleanup(&app.db, &owner);
        let log: Log = Arc::default();
        let sleeper = service(&app.db, &owner);
        sleeper.register_handler(&owner, "w", recorder(&log, 0));
        // A far timer: the loop sleeps its full minute.
        sleeper
            .set(&owner, &request("w:far", in_secs(3600)))
            .await
            .unwrap();
        let (token, handle) = start(&sleeper);
        tokio::time::sleep(Duration::from_millis(1500)).await;

        // Another server's write; this process's loop hears it only by NOTIFY.
        let other = service(&app.db, &owner);
        let at = in_secs(2);
        other.set(&owner, &request("w:near", at)).await.unwrap();
        wait_for(&log, 1, Duration::from_secs(20)).await;
        stop(token, handle).await;

        let fires = log.lock().unwrap().clone();
        assert_eq!(fires.len(), 1, "{fires:?}");
        assert_eq!(fires[0].name, "w:near");
        let late = (fires[0].seen_at - fires[0].run_at).num_milliseconds();
        assert!(
            (0..1000).contains(&late),
            "woke for it: {late} ms late, where the safety recheck would be up to a minute"
        );
    });
}

/// A repeating timer keeps its cadence from its schedule, and after an
/// outage fires once for the slots it missed, not once each.
#[test]
fn a_repeating_timer_keeps_its_cadence_and_fires_once_after_an_outage() {
    common::run_test(async {
        let app = common::shared_app().await;
        let owner = owner();
        cleanup(&app.db, &owner);
        let log: Log = Arc::default();
        let timers = service(&app.db, &owner);
        timers.register_handler(&owner, "r", recorder(&log, 0));

        // Start just after a second begins, so the next slot (+1 s) is ~950 ms
        // away and the catch-up sample below cannot catch it as well.
        let into = Utc::now().timestamp_subsec_millis();
        tokio::time::sleep(Duration::from_millis(u64::from(1050 - into.min(1000)))).await;
        // Scheduled every 2 s from 9 s ago: slots -9, -7, -5, -3 and -1 were
        // missed while no scheduler ran.
        let first = Utc::now().timestamp() - 9;
        let every = TimerRequest {
            repeat_secs: Some(2),
            ..request("r", first)
        };
        timers.set(&owner, &every).await.unwrap();
        let (token, handle) = start(&timers);
        tokio::time::sleep(Duration::from_millis(500)).await;
        let caught_up = log.lock().unwrap().len();
        wait_for(&log, 4, Duration::from_secs(12)).await;
        stop(token, handle).await;

        assert_eq!(caught_up, 1, "the missed slots fire once");
        let fires = log.lock().unwrap().clone();
        assert!(fires.len() >= 4, "{fires:?}");
        assert_eq!(
            fires[0].run_at.timestamp(),
            first,
            "the first fire is the missed slot"
        );
        for pair in fires[1..].windows(2) {
            assert_eq!(
                pair[1].run_at.timestamp() - pair[0].run_at.timestamp(),
                2,
                "every 2 s on the schedule: {fires:?}"
            );
        }
        for f in &fires[1..] {
            assert_eq!(
                (f.run_at.timestamp() - first) % 2,
                0,
                "on the original grid, not counted from a fire time: {f:?}"
            );
            let late = (f.seen_at - f.run_at).num_milliseconds();
            assert!((0..1000).contains(&late), "{late} ms late: {f:?}");
        }
    });
}

/// Each throttle refuses past its limit, with the limit named; nothing is
/// stored for a refused registration.
#[test]
fn each_throttle_refuses_past_its_limit() {
    common::run_test(async {
        let app = common::shared_app().await;
        let owner = owner();
        cleanup(&app.db, &owner);
        let timers = service(&app.db, &owner);
        timers.set_settings_override(Some(TimerSettings {
            max_concurrent: 1,
            max_per_plugin: 3,
            min_repeat_secs: 30,
            max_payload_bytes: 64,
        }));
        let later = in_secs(3600);

        // Payload.
        let big = TimerRequest {
            payload: serde_json::json!({ "text": "x".repeat(100) }),
            ..request("big", later)
        };
        let err = timers
            .set(&owner, &big)
            .await
            .expect_err("payload over 64 bytes");
        assert!(
            matches!(err, TimerRefused::PayloadTooLarge { max: 64, .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("64"), "{err}");

        // Repeat.
        let fast = TimerRequest {
            repeat_secs: Some(10),
            ..request("fast", later)
        };
        let err = timers
            .set(&owner, &fast)
            .await
            .expect_err("repeat under 30 s");
        assert!(
            matches!(err, TimerRefused::RepeatTooShort { min: 30 }),
            "{err:?}"
        );
        assert!(err.to_string().contains("30 seconds"), "{err}");

        // Per owner: three fit, the fourth is refused, and setting one of the
        // three again replaces it rather than counting twice.
        for name in ["a", "b", "c"] {
            timers.set(&owner, &request(name, later)).await.unwrap();
        }
        let err = timers
            .set(&owner, &request("d", later))
            .await
            .expect_err("a fourth timer");
        assert!(
            matches!(err, TimerRefused::TooMany { max: 3, .. }),
            "{err:?}"
        );
        timers
            .set(&owner, &request("a", later + 60))
            .await
            .expect("replacing one held is not a new timer");
        let held: i64 = sqlx::query_scalar("SELECT count(*) FROM scheduled_timer WHERE owner = $1")
            .bind(&owner)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(held, 3, "nothing refused was stored");

        // Concurrency: with max_concurrent 1 the second due timer waits for
        // the first to finish.
        sqlx::query("DELETE FROM scheduled_timer WHERE owner = $1")
            .bind(&owner)
            .execute(&app.db)
            .await
            .unwrap();
        let running = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(AtomicUsize::new(0));
        let (r, m, d) = (running.clone(), most.clone(), done.clone());
        timers.register_handler(
            &owner,
            "slow",
            Arc::new(move |_t: FiredTimer| {
                let (r, m, d) = (r.clone(), m.clone(), d.clone());
                Box::pin(async move {
                    let now = r.fetch_add(1, Ordering::SeqCst) + 1;
                    m.fetch_max(now, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    r.fetch_sub(1, Ordering::SeqCst);
                    d.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        );
        let now = Utc::now().timestamp();
        for i in 0..3 {
            timers
                .set(&owner, &request(&format!("slow:{i}"), now))
                .await
                .unwrap();
        }
        let (token, handle) = start(&timers);
        let until = tokio::time::Instant::now() + Duration::from_secs(10);
        while done.load(Ordering::SeqCst) < 3 && tokio::time::Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        stop(token, handle).await;
        assert_eq!(done.load(Ordering::SeqCst), 3, "all three fired");
        assert_eq!(
            most.load(Ordering::SeqCst),
            1,
            "never more than one at once"
        );
    });
}

/// From the shutdown signal no timer starts; one already firing is waited
/// for and recorded.
#[test]
fn shutdown_starts_no_new_timer_and_waits_for_the_one_firing() {
    common::run_test(async {
        let app = common::shared_app().await;
        let owner = owner();
        cleanup(&app.db, &owner);
        let timers = service(&app.db, &owner);
        let log: Log = Arc::default();
        timers.register_handler(&owner, "later", recorder(&log, 0));
        let finished = Arc::new(AtomicUsize::new(0));
        let f = finished.clone();
        timers.register_handler(
            &owner,
            "long",
            Arc::new(move |_t: FiredTimer| {
                let f = f.clone();
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    f.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }),
        );
        timers
            .set(&owner, &request("long", Utc::now().timestamp()))
            .await
            .unwrap();
        timers
            .set(&owner, &request("later", in_secs(3)))
            .await
            .unwrap();
        let (token, handle) = start(&timers);
        // "long" is firing; the signal comes before "later" is due.
        tokio::time::sleep(Duration::from_millis(500)).await;
        token.cancel();
        tokio::time::timeout(Duration::from_secs(10), handle)
            .await
            .expect("stops once the timer firing is done")
            .unwrap();
        assert_eq!(
            finished.load(Ordering::SeqCst),
            1,
            "the one firing finished"
        );
        assert!(
            status(&app.db, &owner, "long").await.is_none(),
            "and its success was recorded"
        );
        // Past "later"'s time: nothing started it.
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            log.lock().unwrap().is_empty(),
            "no timer starts after the signal"
        );
        let (status, attempts, _) = status(&app.db, &owner, "later").await.expect("still there");
        assert_eq!((status.as_str(), attempts), ("scheduled", 0));
    });
}

/// A handler that keeps failing is retried with backoff, then left `failed`
/// with its error.
#[test]
fn a_failing_handler_retries_then_lands_in_failed() {
    common::run_test(async {
        let app = common::shared_app().await;
        let owner = owner();
        cleanup(&app.db, &owner);
        let timers = service(&app.db, &owner);
        timers.set_retry_backoff(1, 1);
        let log: Log = Arc::default();
        let l = log.clone();
        timers.register_handler(
            &owner,
            "bad",
            Arc::new(move |t: FiredTimer| {
                let l = l.clone();
                Box::pin(async move {
                    l.lock().unwrap().push(Fire {
                        name: t.name,
                        run_at: t.run_at,
                        fired_at: t.fired_at,
                        seen_at: Utc::now(),
                        by: 0,
                        attempt: t.attempt,
                    });
                    Err(format!("broken on attempt {}", t.attempt))
                })
            }),
        );
        timers
            .set(&owner, &request("bad", Utc::now().timestamp()))
            .await
            .unwrap();
        let (token, handle) = start(&timers);
        let until = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let s = status(&app.db, &owner, "bad").await;
            if s.as_ref().is_some_and(|(st, _, _)| st == "failed")
                || tokio::time::Instant::now() > until
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // Nothing more once it has failed.
        tokio::time::sleep(Duration::from_secs(2)).await;
        stop(token, handle).await;

        let fires = log.lock().unwrap().clone();
        let attempts: Vec<i32> = fires.iter().map(|f| f.attempt).collect();
        assert_eq!(
            attempts,
            (1..=MAX_ATTEMPTS).collect::<Vec<_>>(),
            "{fires:?}"
        );
        for pair in fires.windows(2) {
            let gap = (pair[1].fired_at - pair[0].fired_at).num_milliseconds();
            assert!(
                gap >= 900,
                "retried after its backoff, not at once: {gap} ms"
            );
        }
        let (status, attempts, error) = status(&app.db, &owner, "bad").await.expect("kept");
        assert_eq!(status, "failed");
        assert_eq!(attempts, MAX_ATTEMPTS);
        assert_eq!(
            error.as_deref(),
            Some("broken on attempt 3"),
            "the error is kept"
        );
    });
}

/// Registering a name again replaces it: one row, the new time and payload.
#[test]
fn registering_the_same_name_again_replaces_the_timer() {
    common::run_test(async {
        let app = common::shared_app().await;
        let owner = owner();
        cleanup(&app.db, &owner);
        let timers = service(&app.db, &owner);
        timers
            .set(&owner, &request("same", in_secs(100)))
            .await
            .unwrap();
        let replaced = TimerRequest {
            payload: serde_json::json!({ "v": 2 }),
            ..request("same", in_secs(200))
        };
        let t = timers.set(&owner, &replaced).await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM scheduled_timer WHERE owner = $1")
            .bind(&owner)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(t.run_at.timestamp(), replaced.run_at);
        assert_eq!(t.payload, serde_json::json!({ "v": 2 }));
    });
}
