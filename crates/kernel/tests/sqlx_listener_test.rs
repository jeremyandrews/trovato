#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The query listener against sqlx itself (BG14).
//!
//! A statement run through sqlx inside a request profile is counted, and its
//! fingerprint comes from the full statement text (`db.statement`), so the
//! listener is tied to sqlx's actual event and not to a copy of it.
//!
//! Its own binary with a global subscriber: sqlx's statement callsite is
//! shared by every test in a binary, and a thread-local subscriber in a binary
//! of parallel tests can find that callsite's cached interest rebuilt under it.
//!
//! Requires Postgres; runs in CI.

use std::sync::Arc;

use tracing_subscriber::layer::SubscriberExt;
use trovato_kernel::telemetry::RequestProfile;
use trovato_kernel::telemetry::fingerprint::fingerprint;
use trovato_kernel::telemetry::profile::scope;
use trovato_kernel::telemetry::query_listener::query_listener;

/// Longer than sqlx's four-word summary, so the text comes from
/// `db.statement`, as it does for nearly every real statement.
const LONG: &str = "SELECT $1::int4 + 1 AS total, 2 AS other";

#[tokio::test]
async fn a_real_sqlx_statement_is_counted() {
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(query_listener()))
        .expect("one subscriber per test binary");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&trovato_test_utils::env::database_url())
        .await
        .expect("this test needs DATABASE_URL");

    let profile = Arc::new(RequestProfile::new(None, true));
    scope(profile.clone(), async {
        let _: (i32, i32) = sqlx::query_as(LONG)
            .bind(1i32)
            .fetch_one(&pool)
            .await
            .unwrap();
    })
    .await;

    let s = profile.snapshot();
    assert_eq!(s.queries, 1, "{s:?}");
    assert_eq!(s.fingerprints, vec![fingerprint(LONG)]);
}
