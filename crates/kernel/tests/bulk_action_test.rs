#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B68 part 5: bulk actions run as batches, and plugins add their own.
//!
//! A batch used to be a record nothing ran (BL-102): `/api/batch` stored any
//! `operation_type` and the content list's bulk form did its work inline with a
//! hard-coded list of three actions. Now one registry holds the kernel's three
//! and every action a plugin declares under `[[bulk_actions]]`; a submission
//! records a batch and the executor runs it in chunks, re-checking the batch and
//! the submitting user before each chunk, and reporting progress as it goes.
//!
//! The `test_bulk_action` fixture declares `stamp` (needs `stamp content`) and
//! logs every item id its `tap_bulk_action` is handed, with the user it ran as,
//! to `tba_log`.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for, user_holding};
use serde_json::{Value, json};
use uuid::Uuid;

use trovato_kernel::models::CreateItem;

const FIXTURE: &str = "test_bulk_action";
const STAMP: &str = "test_bulk_action.stamp";
const PUBLISH: &str = "publish content";

/// Advisory lock key serializing creation of `tba_log` across test binaries.
const TBA_LOG_LOCK: i64 = 0x0B68_0000_0005;

async fn body_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Scrape the `_token` out of a rendered admin page.
fn token_in(html: &str) -> Option<String> {
    let at = html.find(r#"name="_token""#)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(tag[vat..vend].to_string())
}

/// `GET /admin/content` as the holder of `cookies`: the page and its token.
async fn content_list(app: &TestApp, cookies: &str, bucket: &str) -> (String, String) {
    let page = app
        .request_with_cookies(
            Request::get("/admin/content")
                .header("x-forwarded-for", test_ip_for(bucket))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let html = body_of(page).await;
    let token = token_in(&html).unwrap_or_default();
    (html, token)
}

/// `POST /api/batch` as the holder of `cookies`.
async fn post_batch(
    app: &TestApp,
    cookies: &str,
    bucket: &str,
    operation_type: &str,
    ids: &[Uuid],
) -> (StatusCode, Value) {
    let (_, token) = content_list(app, cookies, bucket).await;
    let response = app
        .request_with_cookies(
            Request::post("/api/batch")
                .header("content-type", "application/json")
                .header("X-CSRF-Token", &token)
                .header("x-forwarded-for", test_ip_for(bucket))
                .body(Body::from(
                    json!({ "operation_type": operation_type, "params": { "item_ids": ids } })
                        .to_string(),
                ))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let body = serde_json::from_str(&body_of(response).await).unwrap_or(Value::Null);
    (status, body)
}

/// `GET /api/batch/{id}`.
async fn batch_status(app: &TestApp, cookies: &str, bucket: &str, id: &str) -> Value {
    let response = app
        .request_with_cookies(
            Request::get(format!("/api/batch/{id}"))
                .header("x-forwarded-for", test_ip_for(bucket))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_str(&body_of(response).await).unwrap()
}

/// Whether a batch has stopped running.
fn finished(batch: &Value) -> bool {
    matches!(
        batch["status"].as_str(),
        Some("complete" | "failed" | "cancelled")
    )
}

/// Poll a batch until it stops running, recording every `processed` seen.
/// A batch still pending after 20 seconds is one nothing will run.
async fn wait_for(app: &TestApp, cookies: &str, bucket: &str, id: &str) -> (Value, Vec<u64>) {
    let mut seen = Vec::new();
    let started = std::time::Instant::now();
    loop {
        let batch = batch_status(app, cookies, bucket, id).await;
        seen.push(batch["progress"]["processed"].as_u64().unwrap_or(0));
        if finished(&batch) {
            return (batch, seen);
        }
        let waited = started.elapsed().as_secs();
        assert!(
            !(batch["status"] == "pending" && waited >= 20),
            "batch {id} was never started: {batch}"
        );
        assert!(
            waited < 300,
            "batch {id} did not finish in five minutes: {batch}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Poll a batch until it has processed at least one chunk.
async fn wait_for_progress(app: &TestApp, cookies: &str, bucket: &str, id: &str) -> Value {
    let started = std::time::Instant::now();
    loop {
        let batch = batch_status(app, cookies, bucket, id).await;
        if batch["progress"]["processed"].as_u64().unwrap_or(0) > 0 || finished(&batch) {
            return batch;
        }
        assert!(
            started.elapsed().as_secs() < 20,
            "batch {id} made no progress in 20 seconds: {batch}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Create `count` `conference` items by `author`, at `status`, through the
/// model (no taps: this is setup, not the thing under test). Removed when the
/// test ends.
async fn seed(app: &TestApp, marker: &str, author: Uuid, status: i16, count: usize) -> Vec<Uuid> {
    app.ensure_conference_type().await;
    let mut ids = Vec::with_capacity(count);
    for n in 0..count {
        let item = trovato_kernel::models::Item::create(
            &app.db,
            CreateItem {
                item_type: "conference".to_string(),
                title: format!("{marker} {n}"),
                author_id: author,
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(json!({})),
                stage_id: None,
                language: Some("en".to_string()),
                log: Some("bulk action test".to_string()),
            },
        )
        .await
        .expect("seed an item");
        ids.push(item.id);
    }
    let pool = app.db.clone();
    let cleanup = ids.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE id = ANY($1)")
            .bind(&cleanup)
            .execute(&pool)
            .await;
    });
    ids
}

async fn statuses(app: &TestApp, ids: &[Uuid]) -> Vec<i16> {
    sqlx::query_scalar("SELECT status FROM item WHERE id = ANY($1)")
        .bind(ids)
        .fetch_all(&app.db)
        .await
        .unwrap()
}

async fn ensure_tba_log(app: &TestApp) {
    let mut tx = app.db.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(TBA_LOG_LOCK)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS tba_log (\
           id BIGSERIAL PRIMARY KEY, item_id TEXT NOT NULL, action TEXT NOT NULL, \
           user_id TEXT NOT NULL)",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

/// What the fixture's tap was handed for `ids`: `(item_id, action, user_id)`.
async fn stamped(app: &TestApp, ids: &[Uuid]) -> Vec<(String, String, String)> {
    let ids: Vec<String> = ids.iter().map(Uuid::to_string).collect();
    sqlx::query_as(
        "SELECT item_id, action, user_id FROM tba_log WHERE item_id = ANY($1) ORDER BY id",
    )
    .bind(&ids)
    .fetch_all(&app.db)
    .await
    .unwrap()
}

/// The content list offers a plugin's action to a user holding the permission
/// it declares, and to nobody else.
#[test]
fn a_plugin_action_is_offered_only_to_a_user_holding_its_permission() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled(FIXTURE).await;
        app.ensure_conference_items().await;

        let (_, stamper) = user_holding(
            app,
            "bulk-offer-yes",
            &["edit any content", "stamp content"],
        )
        .await;
        let (html, _) = content_list(app, &stamper, "bulk-offer-yes").await;
        assert!(
            html.contains(&format!(
                r#"<option value="{STAMP}">Stamp selected</option>"#
            )),
            "a holder of `stamp content` is offered the plugin's action"
        );
        assert!(
            !html.contains(r#"<option value="publish">"#),
            "and not publish, which needs `{PUBLISH}`"
        );

        let (_, editor) = user_holding(app, "bulk-offer-no", &["edit any content"]).await;
        let (html, _) = content_list(app, &editor, "bulk-offer-no").await;
        assert!(
            !html.contains(STAMP),
            "a user without `stamp content` is not offered it"
        );
    });
}

/// The plugin's tap is handed only the items the submitting user may edit, and
/// runs as that user.
#[test]
fn a_plugin_action_is_handed_only_the_items_the_user_may_edit() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled(FIXTURE).await;
        ensure_tba_log(app).await;

        let (user, cookies) = user_holding(
            app,
            "bulk-edit-filter",
            &["stamp content", "edit own content"],
        )
        .await;
        let own = seed(app, "bulk-edit-own", user, 0, 2).await;
        let others = seed(app, "bulk-edit-other", Uuid::nil(), 0, 2).await;
        let selection: Vec<Uuid> = own.iter().chain(&others).copied().collect();

        let (status, body) = post_batch(app, &cookies, "bulk-edit-filter", STAMP, &selection).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let id = body["id"].as_str().unwrap().to_string();
        let (batch, _) = wait_for(app, &cookies, "bulk-edit-filter", &id).await;

        assert_eq!(batch["status"], "complete", "{batch}");
        assert_eq!(batch["result"]["succeeded"], 2, "{batch}");
        assert_eq!(batch["result"]["failed"], 2, "{batch}");

        let log = stamped(app, &selection).await;
        let mut handed: Vec<String> = log.iter().map(|(item, _, _)| item.clone()).collect();
        let mut expected: Vec<String> = own.iter().map(Uuid::to_string).collect();
        handed.sort();
        expected.sort();
        assert_eq!(
            handed, expected,
            "only the user's own items reach the plugin"
        );
        assert!(
            log.iter()
                .all(|(_, action, by)| action == "stamp" && *by == user.to_string()),
            "the tap runs as the submitting user, with the action's own name: {log:?}"
        );
    });
}

/// 2,000 items publish in one batch: progress only moves forward, every item is
/// written once through the item service, and the result is the form's message.
#[test]
fn a_bulk_run_over_two_thousand_items_reports_progress_and_writes_each_item_once() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "bulk-2000", &["edit any content", PUBLISH]).await;
        let ids = seed(app, "bulk-2000", Uuid::nil(), 0, 2000).await;

        let (status, body) = post_batch(app, &cookies, "bulk-2000", "publish", &ids).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let id = body["id"].as_str().unwrap().to_string();
        let (batch, seen) = wait_for(app, &cookies, "bulk-2000", &id).await;

        assert_eq!(batch["status"], "complete", "{batch}");
        assert_eq!(batch["progress"]["total"], 2000);
        assert_eq!(batch["result"]["succeeded"], 2000);
        assert_eq!(batch["result"]["message"], "2000 item(s) published.");
        assert!(
            seen.windows(2).all(|w| w[0] <= w[1]),
            "progress never goes backwards: {seen:?}"
        );
        assert!(
            seen.iter().any(|p| (1..2000).contains(p)),
            "progress was visible part way through: {seen:?}"
        );
        assert!(statuses(app, &ids).await.iter().all(|s| *s == 1));

        // `ItemService::update` records one revision per write, logged with
        // the action; exactly one per item means one service call per item.
        let revisions: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT item_id, count(*) FROM item_revision \
             WHERE item_id = ANY($1) AND log = 'Bulk publish' GROUP BY item_id",
        )
        .bind(&ids)
        .fetch_all(&app.db)
        .await
        .unwrap();
        assert_eq!(revisions.len(), 2000, "every item was written");
        assert!(
            revisions.iter().all(|(_, n)| *n == 1),
            "and written exactly once"
        );
    });
}

/// A content list selection bigger than one chunk runs as a batch, and the
/// list it redirects to polls it.
#[test]
fn a_selection_bigger_than_a_chunk_runs_as_a_batch_the_list_polls() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "bulk-form", &["edit any content", PUBLISH]).await;
        let ids = seed(app, "bulk-form", Uuid::nil(), 0, 150).await;
        let (_, token) = content_list(app, &cookies, "bulk-form").await;

        let mut form = format!("_token={}&action=publish", urlencoding::encode(&token));
        for id in &ids {
            form.push_str(&format!("&ids%5B%5D={id}"));
        }
        let response = app
            .request_with_cookies(
                Request::post("/admin/content/bulk")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("x-forwarded-for", test_ip_for("bulk-form"))
                    .body(Body::from(form))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response.headers()["location"].to_str().unwrap().to_string();
        let id = location
            .strip_prefix("/admin/content?batch=")
            .unwrap_or_else(|| panic!("redirected to {location}"))
            .to_string();

        let page = app
            .request_with_cookies(
                Request::get(&location)
                    .header("x-forwarded-for", test_ip_for("bulk-form"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let html = body_of(page).await;
        assert!(
            html.contains(&format!(r#"data-batch="{id}""#)),
            "the list shows the batch"
        );
        assert!(html.contains(r#"src="/static/js/bulk-progress.js""#));

        let (batch, _) = wait_for(app, &cookies, "bulk-form", &id).await;
        assert_eq!(batch["result"]["message"], "150 item(s) published.");
        assert!(statuses(app, &ids).await.iter().all(|s| *s == 1));
    });
}

/// A user who loses the action's permission while it runs is stopped before
/// the next chunk, and nothing more is written.
#[test]
fn losing_the_permission_stops_the_run() {
    run_test(async {
        let app = shared_app().await;
        let (user, cookies) =
            user_holding(app, "bulk-revoke", &["edit any content", PUBLISH]).await;
        let ids = seed(app, "bulk-revoke", Uuid::nil(), 0, 2000).await;

        let (status, body) = post_batch(app, &cookies, "bulk-revoke", "publish", &ids).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let id = body["id"].as_str().unwrap().to_string();

        // Once a chunk is done, take the permission away.
        let running = wait_for_progress(app, &cookies, "bulk-revoke", &id).await;
        assert!(
            !finished(&running),
            "a 2,000 item run finished before one chunk was seen"
        );
        sqlx::query("DELETE FROM user_roles WHERE user_id = $1")
            .bind(user)
            .execute(&app.db)
            .await
            .unwrap();

        let (after, _) = wait_for(app, &cookies, "bulk-revoke", &id).await;
        assert_eq!(after["status"], "failed", "{after}");
        assert!(
            after["error"]
                .as_str()
                .unwrap_or("")
                .contains("no longer have permission"),
            "{after}"
        );
        let published = statuses(app, &ids)
            .await
            .iter()
            .filter(|s| **s == 1)
            .count();
        assert!(
            published > 0 && published < 2000,
            "the run stopped part way: {published} of 2000 published"
        );
    });
}

/// A batch cancelled while it runs stops before the next chunk.
#[test]
fn a_cancelled_batch_stops() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "bulk-cancel", &["edit any content", PUBLISH]).await;
        let ids = seed(app, "bulk-cancel", Uuid::nil(), 0, 2000).await;

        let (status, body) = post_batch(app, &cookies, "bulk-cancel", "publish", &ids).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let id = body["id"].as_str().unwrap().to_string();
        let running = wait_for_progress(app, &cookies, "bulk-cancel", &id).await;
        assert!(
            !finished(&running),
            "a 2,000 item run finished before one chunk was seen"
        );

        let (_, token) = content_list(app, &cookies, "bulk-cancel").await;
        let response = app
            .request_with_cookies(
                Request::post(format!("/api/batch/{id}/cancel"))
                    .header("X-CSRF-Token", &token)
                    .header("x-forwarded-for", test_ip_for("bulk-cancel"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);

        // The executor notices before its next chunk; give it time to.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let after = batch_status(app, &cookies, "bulk-cancel", &id).await;
        assert_eq!(after["status"], "cancelled", "{after}");
        let published = statuses(app, &ids)
            .await
            .iter()
            .filter(|s| **s == 1)
            .count();
        assert!(
            published < 2000,
            "the run stopped part way: {published} of 2000 published"
        );
    });
}

/// `/api/batch` runs registered actions only, and only for a user holding the
/// action's permissions.
#[test]
fn the_batch_api_refuses_an_unregistered_action_and_an_unpermitted_one() {
    run_test(async {
        let app = shared_app().await;
        let ids = seed(app, "bulk-api", Uuid::nil(), 0, 1).await;
        let (_, cookies) = user_holding(app, "bulk-api", &["edit any content", PUBLISH]).await;

        let (status, body) = post_batch(app, &cookies, "bulk-api", "reindex", &ids).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        let (status, body) = post_batch(app, &cookies, "bulk-api", "delete", &ids).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "delete needs `delete any content`: {body}"
        );
        assert_eq!(statuses(app, &ids).await, vec![0]);
    });
}
