#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `/metrics` counts what it advertises.
//!
//! `cache_hits_total`, `cache_misses_total` and `http_requests_total` were
//! registered with the Prometheus registry and exported on `/metrics`, and
//! nothing ever recorded into them: every kernel cache read went uncounted, and
//! no request was counted either, so the series read zero on a busy site. A
//! dashboard built on them showed a perfectly idle server.
//!
//! These tests read `/metrics` through the router, the way a scraper does, so
//! they fail if the counting is missing anywhere between the read and the text.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Its own client address, so this file does not spend the shared rate-limit
/// bucket that requests without `X-Forwarded-For` fall into.
const CLIENT_IP: &str = "10.76.0.1";

async fn get(app: &TestApp, path: &str) -> (StatusCode, String) {
    let response = app
        .request(
            Request::get(path)
                .header("x-forwarded-for", CLIENT_IP)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// The value of the first sample on `/metrics` whose line starts with
/// `series{` and carries every one of `labels`, or `None` if there is none.
async fn sample(app: &TestApp, series: &str, labels: &[&str]) -> Option<f64> {
    let (status, text) = get(app, "/metrics").await;
    assert_eq!(status, StatusCode::OK, "/metrics answered {status}");
    text.lines()
        .filter(|line| line.starts_with(&format!("{series}{{")))
        .find(|line| labels.iter().all(|l| line.contains(l)))
        .and_then(|line| line.rsplit(' ').next())
        .and_then(|v| v.parse().ok())
}

#[test]
fn loading_an_item_twice_counts_a_miss_then_a_hit() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: "conference".to_string(),
                    title: "Cache metrics fixture".to_string(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({})),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("cache metrics test".to_string()),
                },
                &admin,
            )
            .await
            .expect("create item")
            .id;
        // Whatever creating it left in the cache, the first page load below
        // starts cold.
        app.state.items().invalidate(item);

        let label = "cache=\"item\"";
        let hits_before = sample(app, "cache_hits_total", &[label]).await;
        let misses_before = sample(app, "cache_misses_total", &[label]).await;

        let path = format!("/item/{item}");
        let (first, _) = get(app, &path).await;
        let (second, _) = get(app, &path).await;

        let hits = sample(app, "cache_hits_total", &[label]).await;
        let misses = sample(app, "cache_misses_total", &[label]).await;
        app.state.items().delete(item, &admin).await.ok();

        assert_eq!(first, StatusCode::OK, "first load of {path}");
        assert_eq!(second, StatusCode::OK, "second load of {path}");
        let hits = hits.expect("/metrics has no cache_hits_total{cache=\"item\"} series");
        let misses = misses.expect("/metrics has no cache_misses_total{cache=\"item\"} series");
        assert!(
            misses - misses_before.unwrap_or(0.0) >= 1.0,
            "the cold load counted no item cache miss ({misses_before:?} -> {misses})"
        );
        assert!(
            hits - hits_before.unwrap_or(0.0) >= 1.0,
            "the warm load counted no item cache hit ({hits_before:?} -> {hits})"
        );
    });
}

#[test]
fn a_request_moves_http_requests_total_under_its_route() {
    run_test(async {
        let app = shared_app().await;
        // The route template, never the raw path: an item id in the label
        // would be one series per item.
        let labels = ["method=\"GET\"", "path=\"/user/login\"", "status=\"200\""];
        let before = sample(app, "http_requests_total", &labels).await;

        let (status, _) = get(app, "/user/login").await;
        assert_eq!(status, StatusCode::OK);

        let after = sample(app, "http_requests_total", &labels)
            .await
            .expect("/metrics has no http_requests_total series for GET /user/login 200");
        assert!(
            after - before.unwrap_or(0.0) >= 1.0,
            "GET /user/login was not counted ({before:?} -> {after})"
        );
    });
}

#[test]
fn http_request_labels_use_the_route_template_not_the_path() {
    run_test(async {
        let app = shared_app().await;
        let id = Uuid::new_v4();
        get(app, &format!("/item/{id}")).await;

        let (_, text) = get(app, "/metrics").await;
        let leaked: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("http_request") && l.contains(&id.to_string()))
            .collect();
        assert!(leaked.is_empty(), "a raw path reached a label: {leaked:?}");
        assert!(
            text.lines()
                .any(|l| l.starts_with("http_requests_total{") && l.contains("path=\"/item/{id}\"")),
            "no http_requests_total series for the /item/{{id}} template"
        );
    });
}
