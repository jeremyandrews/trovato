#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Privacy respecting search analytics.
//!
//! Nothing recorded a query: browser searches never reached the server, server
//! searches went through `SearchService::search` and left no trace, and the
//! expansion cache hit and missed silently. These tests pin the aggregator that
//! now counts them: in memory first, flushed to two daily tables, holding the
//! query text with identifying tokens removed and nothing about who searched.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::search::SearchSettings;
use trovato_kernel::search::analytics::normalize_for_analytics;
use uuid::Uuid;

/// Searches counted for `query` across every day, as (searches, zero_results).
async fn counts(app: &TestApp, query: &str) -> (i64, i64) {
    sqlx::query_as(
        "SELECT COALESCE(SUM(searches), 0)::bigint, COALESCE(SUM(zero_results), 0)::bigint \
         FROM search_query_daily WHERE query = $1",
    )
    .bind(query)
    .fetch_one(&app.db)
    .await
    .expect("read the daily aggregate")
}

async fn search_page(app: &TestApp, query: &str) -> StatusCode {
    let uri = format!("/search?q={}", urlencoding::encode(query));
    app.request(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .status()
}

// =============================================================================
// (a) A recorded search reaches the daily aggregate after a flush
// =============================================================================

#[test]
fn a_search_is_in_the_daily_aggregate_after_a_flush() {
    run_test(async {
        let app = shared_app().await;
        let analytics = app.state.search().analytics();
        let query = format!("zqxnothing{}", Uuid::now_v7().simple());

        // A server search that finds nothing: one search, one zero result.
        assert_eq!(search_page(app, &query).await, StatusCode::OK);
        // And the same query recorded from the browser with three results.
        analytics.record(&SearchSettings::default(), &query, 3);

        analytics.flush().await.expect("flush");

        assert_eq!(
            counts(app, &query).await,
            (2, 1),
            "two searches for the query, one of which found nothing"
        );

        // A second flush with nothing pending adds nothing.
        analytics.flush().await.expect("flush");
        assert_eq!(counts(app, &query).await, (2, 1));
    });
}

/// With analytics switched off, recording stores nothing.
#[test]
fn nothing_is_recorded_while_analytics_is_off() {
    run_test(async {
        let app = shared_app().await;
        let analytics = app.state.search().analytics();
        let query = format!("zqxoff{}", Uuid::now_v7().simple());

        let off = SearchSettings {
            analytics_enabled: false,
            ..SearchSettings::default()
        };
        analytics.record(&off, &query, 0);
        analytics.flush().await.expect("flush");

        assert_eq!(counts(app, &query).await, (0, 0));
    });
}

// =============================================================================
// (b) Recording never waits on the database
// =============================================================================

#[test]
fn a_search_returns_while_the_flush_is_blocked() {
    run_test(async {
        let app = shared_app().await;
        let analytics = app.state.search().analytics();
        let query = format!("zqxblocked{}", Uuid::now_v7().simple());

        // Hold the writer: any flush now waits on this guard.
        let guard = analytics.lock_flush().await;

        // A flush started now is stuck behind the guard.
        let pending_flush = tokio::spawn(async move { analytics.flush().await });

        // The record call is synchronous: it returns without awaiting anything.
        analytics.record(&SearchSettings::default(), &query, 1);

        // And a whole search request completes at once.
        let status = tokio::time::timeout(Duration::from_secs(10), search_page(app, &query))
            .await
            .expect("a search must not wait on the analytics writer");
        assert_eq!(status, StatusCode::OK);
        assert!(!pending_flush.is_finished(), "the flush is still blocked");

        drop(guard);
        pending_flush.await.unwrap().expect("flush");
        analytics.flush().await.expect("flush");

        assert_eq!(counts(app, &query).await.0, 2, "both searches were kept");
    });
}

// =============================================================================
// (c) What is stored
// =============================================================================

#[test]
fn identifying_tokens_are_replaced_before_anything_is_stored() {
    assert_eq!(
        normalize_for_analytics("  Contact  Jane.Doe+news@Example.co.uk about invoice 12345678 "),
        Some("contact [email] about invoice [number]".to_string())
    );
    // Five digits is a number someone might search for (a postcode, a year
    // range); six or more is treated as an identifier.
    assert_eq!(
        normalize_for_analytics("Route 12345"),
        Some("route 12345".to_string())
    );
    assert_eq!(
        normalize_for_analytics("call 0123456"),
        Some("call [number]".to_string())
    );
    assert_eq!(normalize_for_analytics("   \t "), None);

    // At most 100 characters, cut on a character boundary.
    let long = "é".repeat(150);
    let stored = normalize_for_analytics(&long).unwrap();
    assert_eq!(stored.chars().count(), 100);
}

#[test]
fn a_recorded_query_is_stored_normalized() {
    run_test(async {
        let app = shared_app().await;
        let analytics = app.state.search().analytics();
        let stamp = Uuid::now_v7().simple().to_string();

        analytics.record(
            &SearchSettings::default(),
            &format!("Mail ME@example.com {stamp} 9876543"),
            0,
        );
        analytics.flush().await.expect("flush");

        assert_eq!(
            counts(app, &format!("mail [email] {stamp} [number]")).await,
            (1, 1)
        );
        let leaked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM search_query_daily WHERE query LIKE '%example.com%' \
             OR query LIKE '%9876543%'",
        )
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(leaked, 0, "the raw tokens are never stored");
    });
}

// =============================================================================
// (d) Retention
// =============================================================================

#[test]
fn rows_older_than_the_retention_window_are_deleted() {
    run_test(async {
        let app = shared_app().await;
        let analytics = app.state.search().analytics();
        let old = format!("zqxold{}", Uuid::now_v7().simple());
        let recent = format!("zqxrecent{}", Uuid::now_v7().simple());

        for (query, age) in [(&old, 40), (&recent, 5)] {
            sqlx::query(
                "INSERT INTO search_query_daily (day, query, searches, zero_results) \
                 VALUES ((now() AT TIME ZONE 'UTC')::date - $2::int, $1, 1, 0)",
            )
            .bind(query)
            .bind(age)
            .execute(&app.db)
            .await
            .unwrap();
        }
        // A daily counter row past the window, on a day no other test writes.
        sqlx::query(
            "INSERT INTO search_daily (day, expansion_hits) \
             VALUES ((now() AT TIME ZONE 'UTC')::date - 400, 1) ON CONFLICT (day) DO NOTHING",
        )
        .execute(&app.db)
        .await
        .unwrap();

        analytics.prune(30).await.expect("prune");

        assert_eq!(counts(app, &old).await, (0, 0), "older than 30 days: gone");
        assert_eq!(counts(app, &recent).await, (1, 0), "newer: kept");
        let ancient: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM search_daily \
             WHERE day <= (now() AT TIME ZONE 'UTC')::date - 400",
        )
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(ancient, 0, "search_daily is pruned too");
    });
}

// =============================================================================
// Expansion cache counters
// =============================================================================

/// A cached expansion counts as a hit, an uncached one as a miss.
#[test]
fn expansion_cache_hits_and_misses_are_counted() {
    use trovato_kernel::models::SiteConfig;
    use trovato_kernel::routes::api_search::{EXPANSION_CACHE_TAG, expansion_cache_key};

    run_test(async {
        let app = shared_app().await;
        let analytics = app.state.search().analytics();
        analytics.flush().await.expect("flush what is pending");

        let today = || async {
            sqlx::query_as::<_, (i64, i64)>(
                "SELECT COALESCE(SUM(expansion_hits), 0)::bigint, \
                        COALESCE(SUM(expansion_misses), 0)::bigint \
                 FROM search_daily WHERE day = (now() AT TIME ZONE 'UTC')::date",
            )
            .fetch_one(&app.db)
            .await
            .unwrap()
        };
        let (hits_before, misses_before) = today().await;

        let name = SiteConfig::site_name(&app.db)
            .await
            .unwrap_or_else(|_| "Trovato".to_string());
        let slogan = SiteConfig::site_slogan(&app.db).await.unwrap_or_default();
        let cached = format!("zqxhit {}", Uuid::now_v7().simple());
        let key = expansion_cache_key(&cached, &name, &slogan);
        app.state
            .cache()
            .set(&key, "[\"alpha\"]", 300, &[EXPANSION_CACHE_TAG])
            .await;

        for query in [
            cached.clone(),
            format!("zqxmiss {}", Uuid::now_v7().simple()),
        ] {
            app.request(
                Request::post("/api/v1/search/expand")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "query": query }).to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        }
        analytics.flush().await.expect("flush");

        let (hits, misses) = today().await;
        assert!(hits > hits_before, "the cached query is a hit");
        assert!(misses > misses_before, "the uncached query is a miss");

        app.state.cache().invalidate(&key).await;
    });
}
