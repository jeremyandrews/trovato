#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Timers at scale (B278): 10,000 timers in the table, measured. Ignored by
//! default; run it on purpose against a quiet database:
//!
//! ```sh
//! cargo test -p trovato-kernel --test timer_bench_test -- --ignored --nocapture
//! ```
//!
//! It prints, and docs/benchmarks/timers.md records:
//!
//! - the claim and wake queries' plans (`EXPLAIN (ANALYZE, BUFFERS)`), the
//!   statements the scheduler runs, unscoped as in production;
//! - each query's time, p50 and p95 over 200 runs;
//! - wake latency: a timer set due now by another connection, from its
//!   `NOTIFY` to its handler starting, while the loop sleeps toward a timer
//!   an hour away;
//! - lateness: 1,000 timers due over ten seconds among 9,000 later ones, run
//!   by two schedulers, each fire's lateness p50, p95 and max, and that each
//!   fired once.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use trovato_kernel::timer::{
    self, CLAIM_SQL, FiredTimer, NEXT_DUE_SQL, TimerRequest, TimerService, TimerSettings,
};

const TOTAL: i64 = 10_000;

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i]
}

fn summary(label: &str, mut samples: Vec<f64>, unit: &str) -> (f64, f64, f64) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let (p50, p95, max) = (
        percentile(&samples, 0.5),
        percentile(&samples, 0.95),
        *samples.last().unwrap(),
    );
    println!(
        "{label}: n={} p50={p50:.2}{unit} p95={p95:.2}{unit} max={max:.2}{unit}",
        samples.len()
    );
    (p50, p95, max)
}

/// `count` timers for `owner`, named `<prefix>:<i>`, the first due at
/// `first_ms` from now and the rest `step_ms` apart.
async fn bulk(pool: &PgPool, owner: &str, prefix: &str, count: i64, first_ms: i64, step_ms: i64) {
    sqlx::query(
        "INSERT INTO scheduled_timer (owner, name, run_at, slot_at) \
         SELECT $1, $2 || ':' || i, \
                date_trunc('second', now()) + make_interval(secs => ($3 + i * $4) / 1000.0), \
                date_trunc('second', now()) + make_interval(secs => ($3 + i * $4) / 1000.0) \
         FROM generate_series(0, $5 - 1) AS i",
    )
    .bind(owner)
    .bind(prefix)
    .bind(first_ms as f64)
    .bind(step_ms as f64)
    .bind(count)
    .execute(pool)
    .await
    .unwrap();
}

async fn explain(pool: &PgPool, label: &str, sql: &str, claim: bool) {
    let mut tx = pool.begin().await.unwrap();
    let explain = format!("EXPLAIN (ANALYZE, BUFFERS, COSTS OFF, TIMING ON) {sql}");
    let q = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(explain)).bind(None::<Vec<String>>);
    let q = if claim {
        q.bind(4_i64).bind("bench").bind(960.0_f64)
    } else {
        q
    };
    let plan: Vec<String> = q.fetch_all(&mut *tx).await.unwrap();
    tx.rollback().await.unwrap();
    println!("\n--- EXPLAIN {label} ---\n{}\n", plan.join("\n"));
}

/// Time `sql` `n` times, each in a rolled back transaction so a claim
/// claims nothing for good.
async fn time_query(pool: &PgPool, sql: &str, claim: bool, n: usize) -> Vec<f64> {
    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        let mut tx = pool.begin().await.unwrap();
        let started = Instant::now();
        if claim {
            sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
                .bind(None::<Vec<String>>)
                .bind(4_i64)
                .bind("bench")
                .bind(960.0_f64)
                .execute(&mut *tx)
                .await
                .unwrap();
        } else {
            sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
                .bind(None::<Vec<String>>)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
        tx.rollback().await.unwrap();
    }
    samples
}

#[test]
#[ignore = "a benchmark: run with --ignored against a quiet database"]
fn ten_thousand_timers() {
    common::run_test(async {
        let app = common::shared_app().await;
        let pool = app.db.clone();
        let owner = format!("bench_{}", &Uuid::new_v4().simple().to_string()[..10]);
        {
            let pool = pool.clone();
            let owner = owner.clone();
            common::defer_cleanup(move || async move {
                let _ = sqlx::query("DELETE FROM scheduled_timer WHERE owner = $1")
                    .bind(owner)
                    .execute(&pool)
                    .await;
            });
        }

        // --- The queries, on 10,000 timers: 100 due, 9,900 over the next day.
        bulk(&pool, &owner, "due", 100, -60_000, 10).await;
        bulk(&pool, &owner, "later", TOTAL - 100, 3_600_000, 8_000).await;
        sqlx::query("ANALYZE scheduled_timer")
            .execute(&pool)
            .await
            .unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM scheduled_timer")
            .fetch_one(&pool)
            .await
            .unwrap();
        println!("scheduled_timer rows: {rows}");

        explain(&pool, "claim (limit 4, unscoped)", CLAIM_SQL, true).await;
        explain(&pool, "next due (unscoped)", NEXT_DUE_SQL, false).await;
        // Warm, then measure.
        time_query(&pool, CLAIM_SQL, true, 20).await;
        time_query(&pool, NEXT_DUE_SQL, false, 20).await;
        summary(
            "claim query",
            time_query(&pool, CLAIM_SQL, true, 200).await,
            " ms",
        );
        summary(
            "next-due query",
            time_query(&pool, NEXT_DUE_SQL, false, 200).await,
            " ms",
        );
        sqlx::query("DELETE FROM scheduled_timer WHERE owner = $1 AND name LIKE 'due:%'")
            .bind(&owner)
            .execute(&pool)
            .await
            .unwrap();

        // --- Wake latency: the loop sleeps toward a timer an hour away; another
        // connection sets one due now, and only its NOTIFY wakes the loop.
        let settings = TimerSettings {
            max_per_plugin: 100_000,
            min_repeat_secs: 1,
            ..TimerSettings::default()
        };
        let seen: Arc<Mutex<HashMap<String, (chrono::DateTime<Utc>, usize)>>> = Arc::default();
        let fires: Arc<Mutex<Vec<(String, usize, f64)>>> = Arc::default();
        let make = |by: usize| {
            let timers = Arc::new(TimerService::new(pool.clone(), None));
            timers.set_owner_scope(Some(vec![owner.clone()]));
            timers.set_settings_override(Some(settings));
            let seen = seen.clone();
            let fires = fires.clone();
            timers.register_handler(
                &owner,
                "w",
                Arc::new(move |t: FiredTimer| {
                    let seen = seen.clone();
                    let fires = fires.clone();
                    Box::pin(async move {
                        seen.lock()
                            .unwrap()
                            .insert(t.name.clone(), (Utc::now(), by));
                        let late =
                            (Utc::now() - t.run_at).num_microseconds().unwrap_or(0) as f64 / 1000.0;
                        fires.lock().unwrap().push((t.name, by, late));
                        Ok(())
                    })
                }),
            );
            timers
        };
        let a = make(0);
        let b = make(1);
        let token = CancellationToken::new();
        let ha = tokio::spawn(a.clone().run_scheduler(token.clone()));
        let hb = tokio::spawn(b.clone().run_scheduler(token.clone()));
        tokio::time::sleep(Duration::from_secs(2)).await;

        let mut wake = Vec::new();
        for i in 0..30 {
            let name = format!("w:wake{i}");
            let request = TimerRequest {
                name: name.clone(),
                run_at: Utc::now().timestamp(),
                repeat_secs: None,
                payload: serde_json::json!({}),
            };
            // Through the module, not a service: no in-process wake, only NOTIFY.
            let set_at = Utc::now();
            timer::set(&pool, &owner, &request, &settings)
                .await
                .unwrap();
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some((at, _)) = seen.lock().unwrap().get(&name) {
                    wake.push((*at - set_at).num_microseconds().unwrap() as f64 / 1000.0);
                    break;
                }
                assert!(Instant::now() < until, "{name} never fired");
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        summary(
            "wake latency (set due now by another connection, to handler start)",
            wake,
            " ms",
        );

        // --- Lateness: 1,000 due over ten seconds, from two seconds on,
        // among 9,000 later ones; two schedulers.
        fires.lock().unwrap().clear();
        // 9,000 later ones and 1,000 new: 10,000 in all.
        sqlx::query(
            "DELETE FROM scheduled_timer WHERE id IN (SELECT id FROM scheduled_timer \
             WHERE owner = $1 AND name LIKE 'later:%' ORDER BY run_at DESC LIMIT 900)",
        )
        .bind(&owner)
        .execute(&pool)
        .await
        .unwrap();
        bulk(&pool, &owner, "w", 1_000, 2_000, 10).await;
        let total: i64 =
            sqlx::query_scalar("SELECT count(*) FROM scheduled_timer WHERE owner = $1")
                .bind(&owner)
                .fetch_one(&pool)
                .await
                .unwrap();
        println!("timers for the lateness run: {total}");
        // Wake both loops for the new rows, as a set() would.
        sqlx::query("SELECT pg_notify($1, '0')")
            .bind(timer::NOTIFY_CHANNEL)
            .execute(&pool)
            .await
            .unwrap();
        let until = Instant::now() + Duration::from_secs(60);
        while fires.lock().unwrap().len() < 1_000 && Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        token.cancel();
        ha.await.unwrap();
        hb.await.unwrap();

        let fired = fires.lock().unwrap().clone();
        let mut per_name: HashMap<&str, usize> = HashMap::new();
        for (name, _, _) in &fired {
            *per_name.entry(name.as_str()).or_default() += 1;
        }
        let bulk_names = per_name.keys().filter(|n| !n.starts_with("w:wake")).count();
        let doubles = per_name.values().filter(|n| **n > 1).count();
        let by_a = fired.iter().filter(|(_, by, _)| *by == 0).count();
        println!(
            "fires: {} for {bulk_names} timers, {doubles} fired more than once; server A {by_a}, server B {}",
            fired.len(),
            fired.len() - by_a
        );
        assert_eq!(bulk_names, 1_000, "every timer fired");
        assert_eq!(doubles, 0, "none fired twice");

        let late: Vec<f64> = fired
            .iter()
            .filter(|(name, _, _)| !name.starts_with("w:wake"))
            .map(|(_, _, late)| *late)
            .collect();
        summary(
            "lateness (1,000 due over 10 s among 10,000; due to handler start)",
            late,
            " ms",
        );
    });
}
