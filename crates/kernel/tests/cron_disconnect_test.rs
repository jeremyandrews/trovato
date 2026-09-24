#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A cron run must outlive the request that triggered it.
//!
//! The poker allows each run 30 seconds (`curl -m 30` in the compose file). When
//! a run outlasts that the client hangs up, and while the handler awaited
//! `CronService::run` inline that disconnect dropped the run itself part way
//! through: its queue jobs were aborted mid provider call with their rows left
//! `claimed`, and the lock its `release_lock` would have freed stayed held until
//! the TTL expired.

mod common;

use axum::body::Body;
use axum::http::Request;
use std::time::Duration;

/// Mirrors the private `CRON_LOCK_KEY` in `crates/kernel/src/cron/mod.rs`.
const CRON_LOCK_KEY: &str = "cron:lock";

/// Dropping the request must not cancel the run.
///
/// The client disconnects while the run is still going, and the run has to
/// finish anyway and give the lock back. Red before the route detached the run:
/// the dropped future took the run with it, so nothing ever released the lock
/// and it sat there for its whole 300-second TTL.
#[test]
fn a_disconnected_client_does_not_cancel_the_cron_run() {
    common::run_test(async {
        let app = common::shared_app().await;
        let key = app.state.runtime().cron_key.clone();

        let redis_url =
            std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let client = redis::Client::open(redis_url).expect("redis client");
        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .expect("redis connection");

        // Start from a clean lock so the assertion below is about this run.
        let _: () = redis::cmd("DEL")
            .arg(CRON_LOCK_KEY)
            .query_async(&mut conn)
            .await
            .unwrap_or(());

        // Fire the run, then walk away from it as a disconnecting client does.
        // The timeout is short rather than the poker's 30 seconds on purpose: a
        // test-sized run finishes in well under a second, so waiting a second
        // here would prove nothing — the run would already be over and there
        // would be no cancellation to survive. What matters is that the future
        // is dropped *while the run is still going*, which a short timeout
        // guarantees and a long one does not.
        {
            let request = Request::builder()
                .method("POST")
                .uri(format!("/cron/{key}"))
                .body(Body::empty())
                .unwrap();
            let mut pending = Box::pin(app.request(request));
            let early = tokio::time::timeout(Duration::from_millis(5), &mut pending).await;
            assert!(
                early.is_err(),
                "the run finished before the client disconnected, so this test \
                 exercised nothing; make the run slower or the timeout shorter"
            );
            // `pending` drops here: the client is gone.
        }

        // A detached run finishes and releases the lock. A cancelled one never
        // reaches `release_lock`, so the key survives on its 300-second TTL.
        let mut released = false;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let held: Option<String> = redis::cmd("GET")
                .arg(CRON_LOCK_KEY)
                .query_async(&mut conn)
                .await
                .unwrap_or(None);
            if held.is_none() {
                released = true;
                break;
            }
        }

        let ttl: i64 = redis::cmd("TTL")
            .arg(CRON_LOCK_KEY)
            .query_async(&mut conn)
            .await
            .unwrap_or(-2);

        assert!(
            released,
            "the cron lock was still held 10 seconds after the client \
             disconnected (TTL {ttl}): the run was cancelled with the request \
             instead of outliving it, so it never released what it held"
        );
    });
}
