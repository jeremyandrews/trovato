#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The AI query expansion cache.
//!
//! Every call to `/api/v1/search/expand` went to the configured LLM provider, so
//! identical queries re-billed every time. `docs/design/search-architecture.md`
//! specified an expansion cache; none was built.
//!
//! These tests exercise the cache from the outside, using the fact that the
//! endpoint reaches the cache *before* it resolves a provider: with no provider
//! configured, a cached expansion is the difference between 200 and 503. That is a
//! sharper signal than counting provider calls, and it needs no provider.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::SiteConfig;
use trovato_kernel::routes::api_search::{EXPANSION_CACHE_TAG, expansion_cache_key};
use uuid::Uuid;

async fn post_expand(app: &TestApp, query: &str) -> (StatusCode, String) {
    let response = app
        .request(
            Request::post("/api/v1/search/expand")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"query": query}).to_string()))
                .unwrap(),
        )
        .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 256 * 1024)
        .await
        .expect("read body");

    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The site name and slogan the running app will resolve, so a test key matches
/// the key the handler builds.
async fn prompt_inputs(app: &TestApp) -> (String, String) {
    let name = SiteConfig::site_name(&app.db)
        .await
        .unwrap_or_else(|_| "Trovato".to_string());
    let slogan = SiteConfig::site_slogan(&app.db).await.unwrap_or_default();
    (name, slogan)
}

async fn seed(app: &TestApp, query: &str, terms: &[&str]) -> String {
    let (name, slogan) = prompt_inputs(app).await;
    let key = expansion_cache_key(query, &name, &slogan);
    let payload = serde_json::to_string(terms).unwrap();

    app.state
        .cache()
        .set(&key, &payload, 300, &[EXPANSION_CACHE_TAG])
        .await;

    key
}

/// A cached expansion is served without touching a provider.
#[test]
fn a_cached_expansion_is_served_without_a_provider() {
    run_test(async {
        let app = shared_app().await;
        let query = format!("cached query {}", Uuid::now_v7().simple());
        let key = seed(app, &query, &["alpha", "beta"]).await;

        let (status, body) = post_expand(app, &query).await;

        assert_eq!(
            status,
            StatusCode::OK,
            "a cached expansion must be served; body was {body}"
        );
        assert!(body.contains("alpha") && body.contains("beta"), "{body}");

        app.state.cache().invalidate(&key).await;
    });
}

/// With nothing cached, the request goes on to the provider — and says so when
/// there is none. This is what makes the test above meaningful.
#[test]
fn an_uncached_query_still_reaches_for_a_provider() {
    run_test(async {
        let app = shared_app().await;
        let query = format!("uncached query {}", Uuid::now_v7().simple());

        let (status, _) = post_expand(app, &query).await;

        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "with no provider configured and no cache entry, the endpoint reports \
             the missing provider rather than inventing an answer"
        );
    });
}

/// The same question asked with different spacing and case is one cache entry, so
/// the second asker does not pay for another provider call.
#[test]
fn spacing_and_case_do_not_split_the_cache() {
    run_test(async {
        let app = shared_app().await;
        let stamp = Uuid::now_v7().simple().to_string();
        let key = seed(app, &format!("rust async {stamp}"), &["tokio"]).await;

        let (status, body) = post_expand(app, &format!("  RUST   Async   {stamp} ")).await;

        assert_eq!(status, StatusCode::OK, "body was {body}");
        assert!(body.contains("tokio"), "{body}");

        app.state.cache().invalidate(&key).await;
    });
}

/// An expansion is produced from a prompt built with the site's name and slogan,
/// so a renamed site must not be served the old site's expansions.
#[test]
fn a_renamed_site_does_not_reuse_the_old_expansion() {
    run_test(async {
        let app = shared_app().await;
        let query = format!("branding query {}", Uuid::now_v7().simple());

        // Seeded under a name that is not the site's.
        let key = expansion_cache_key(&query, "A Completely Different Site", "");
        app.state
            .cache()
            .set(&key, "[\"stale\"]", 300, &[EXPANSION_CACHE_TAG])
            .await;

        let (status, body) = post_expand(app, &query).await;

        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "the entry belongs to another site's prompt and must be a miss; body was {body}"
        );

        app.state.cache().invalidate(&key).await;
    });
}

/// An empty query is still rejected before any cache work.
#[test]
fn an_empty_query_is_refused() {
    run_test(async {
        let app = shared_app().await;

        let (status, _) = post_expand(app, "   ").await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
    });
}
