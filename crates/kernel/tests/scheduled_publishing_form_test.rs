#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Scheduled publishing on the item form (B326).
//!
//! Before this, `trovato_scheduled_publishing` read `field_publish_on` /
//! `field_unpublish_on` as Unix seconds, but nothing wrote them: no content
//! type declared them and the item form had no input for them. These drive
//! the real router (`/item/add/page`, `/item/{id}/edit`) with the plugin's own
//! WASM loaded, proving the whole path: the "Publishing schedule" group
//! (`tap_form_alter`), its refusals (`tap_form_validate`, on both the form and
//! the JSON item API), its write and timer (`tap_form_submit`), and the timer
//! actually publishing the item.
//!
//! Requires Postgres + Redis and `plugins/trovato_scheduled_publishing/trovato_scheduled_publishing.wasm`
//! (`scripts/ci-build-plugins.sh`).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::TestApp;
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

const PLUGIN: &str = "trovato_scheduled_publishing";
const PERM: &str = "schedule publishing";
const TYPE: &str = "page";

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("scheduled publishing fixture app init thread panicked")
    })
}

async fn build_app() -> TestApp {
    trovato_test_utils::env::load_dotenv();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("failed to connect for fixture setup");
    trovato_kernel::plugin::status::install_plugin(&pool, PLUGIN, "1.0.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{PLUGIN}': {e:#}"));
    pool.close().await;

    TestApp::with_config(|config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
    })
    .await
}

/// Put the plugin's status back so it does not load in other test binaries.
async fn restore_plugin(app: &TestApp) {
    sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = $1")
        .bind(PLUGIN)
        .execute(&app.db)
        .await
        .ok();
}

/// Run one test against the fixture app, restoring plugin status after.
fn run<F, Fut>(test: F)
where
    F: FnOnce(&'static TestApp) -> Fut + Send,
    Fut: std::future::Future<Output = ()> + Send,
{
    common::run_test(async {
        let app = app();
        test(app).await;
        restore_plugin(app).await;
    });
}

// =============================================================================
// HTTP helpers (each integration test file in this suite keeps its own small
// set rather than share one; see `form_pipeline_test.rs` for the same shape).
// =============================================================================

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

fn input_value(html: &str, name: &str) -> Option<String> {
    common::input_value_in(html, name)
}

fn form_tokens(html: &str) -> String {
    let token = input_value(html, "_token").expect("the form carries a CSRF token");
    let build = input_value(html, "_form_build_id").expect("the form carries a build id");
    format!(
        "_token={}&_form_build_id={}",
        urlencoding::encode(&token),
        urlencoding::encode(&build)
    )
}

async fn get_page(app: &TestApp, cookies: &str, uri: &str) -> String {
    let page = app
        .request_with_cookies(
            Request::builder().uri(uri).body(Body::empty()).unwrap(),
            cookies,
        )
        .await;
    body_string(page).await
}

async fn post_form(app: &TestApp, cookies: &str, uri: &str, body: String) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn post_json(
    app: &TestApp,
    cookies: &str,
    uri: &str,
    csrf: &str,
    body: Value,
) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .header("X-CSRF-Token", csrf)
                .body(Body::from(body.to_string()))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn set_timezone(app: &TestApp, user_id: Uuid, zone: &str) {
    sqlx::query("UPDATE users SET timezone = $1 WHERE id = $2")
        .bind(zone)
        .bind(user_id)
        .execute(&app.db)
        .await
        .unwrap();
}

async fn stored_fields(app: &TestApp, id: Uuid) -> Value {
    sqlx::query_scalar("SELECT fields FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

async fn item_status(app: &TestApp, id: Uuid) -> i16 {
    sqlx::query_scalar("SELECT status FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// Seed a `page` item directly through the item service (an administrator
/// bypass, not under test), with whatever `fields` the test wants already
/// stored.
async fn seed_page(app: &TestApp, author: Uuid, status: i16, fields: Value) -> Uuid {
    let user = trovato_kernel::tap::UserContext::administrator(author, vec![]);
    app.state
        .items()
        .create(
            trovato_kernel::models::CreateItem {
                item_type: TYPE.to_string(),
                title: "Schedule probe".to_string(),
                author_id: author,
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(fields),
                stage_id: None,
                language: None,
                log: None,
            },
            &user,
        )
        .await
        .expect("seed item")
        .id
}

/// An editor who may create and edit a `page` and schedule its publishing,
/// in `zone`.
async fn editor(app: &TestApp, prefix: &str, zone: &str) -> (Uuid, String) {
    let (uid, cookies) = common::user_holding(
        app,
        prefix,
        &[
            "create page content",
            "edit any content",
            "publish content",
            PERM,
        ],
    )
    .await;
    set_timezone(app, uid, zone).await;
    (uid, cookies)
}

async fn timer_run_at(app: &TestApp, item_id: Uuid, prefix: &str) -> Option<i64> {
    trovato_kernel::timer::get(&app.db, PLUGIN, &format!("{prefix}:{item_id}"))
        .await
        .unwrap()
        .map(|t| t.run_at.timestamp())
}

// =============================================================================
// The group itself
// =============================================================================

/// The group's two inputs, each labelled with the viewer's time zone, show
/// only for a holder of `schedule publishing`.
#[test]
fn the_group_shows_only_with_the_permission() {
    run(|app| async move {
        let (_uid, with) = editor(app, "b326with", "Europe/Rome").await;
        let (_uid2, without) = common::user_holding(
            app,
            "b326without",
            &["create page content", "edit any content"],
        )
        .await;

        let html = get_page(app, &with, &format!("/item/add/{TYPE}")).await;
        assert!(
            html.contains(r#"name="schedule_publish_on""#),
            "a holder of {PERM} must see the publish input: {html}"
        );
        assert!(html.contains(r#"name="schedule_unpublish_on""#));
        assert!(
            html.contains("Europe/Rome"),
            "the input must name the time zone it reads: {html}"
        );

        let html = get_page(app, &without, &format!("/item/add/{TYPE}")).await;
        assert!(
            !html.contains("schedule_publish_on"),
            "without {PERM} the group must not render at all: {html}"
        );
    });
}

/// Reopening the edit form shows what is scheduled, in the viewer's zone.
#[test]
fn the_edit_form_shows_the_stored_schedule_in_the_viewers_zone() {
    run(|app| async move {
        let (uid, cookies) = editor(app, "b326reopen", "Europe/Rome").await;
        // 2027-06-15 12:00 Europe/Rome (CEST, UTC+2) is 2027-06-15 10:00 UTC.
        let publish_at: i64 = 1_813_053_600;
        let id = seed_page(
            app,
            uid,
            0,
            json!({"body": "x", "field_publish_on": publish_at}),
        )
        .await;

        let html = get_page(app, &cookies, &format!("/item/{id}/edit")).await;
        assert_eq!(
            input_value(&html, "schedule_publish_on").as_deref(),
            Some("2027-06-15T12:00"),
            "the stored UTC instant must read back in the viewer's own zone: {html}"
        );
        assert!(
            html.contains("Scheduled to publish on"),
            "a status line must say what is scheduled: {html}"
        );
    });
}

// =============================================================================
// Saving the form: conversion and the timer
// =============================================================================

/// A save in Europe/Rome converts the typed local time to the correct UTC
/// seconds and sets the `publish:<id>` timer for exactly that second.
#[test]
fn a_save_in_europe_rome_stores_utc_seconds_and_sets_the_timer() {
    run(|app| async move {
        let (uid, cookies) = editor(app, "b326rome", "Europe/Rome").await;
        let id = seed_page(app, uid, 0, json!({"body": "x"})).await;

        let edit_path = format!("/item/{id}/edit");
        let html = get_page(app, &cookies, &edit_path).await;
        // No `status`: the item stays unpublished, as seeded, the only state
        // a publish date can usefully arm a timer from.
        let body = format!(
            "{}&title=Schedule+probe&body=x&schedule_publish_on=2027-06-15T12%3A00",
            form_tokens(&html)
        );
        let (status, text) = post_form(app, &cookies, &edit_path, body).await;
        assert!(status.is_success(), "{status}: {text}");

        // 2027-06-15 12:00 CEST (UTC+2) == 2027-06-15 10:00 UTC.
        let expected: i64 = 1_813_053_600;
        assert_eq!(
            stored_fields(app, id).await["field_publish_on"],
            json!(expected)
        );
        assert_eq!(timer_run_at(app, id, "publish").await, Some(expected));
    });
}

/// Clearing the input removes the stored key and cancels the timer.
#[test]
fn clearing_the_input_removes_the_key_and_cancels_the_timer() {
    run(|app| async move {
        let (uid, cookies) = editor(app, "b326clear", "UTC").await;
        let future = chrono::Utc::now().timestamp() + 3600;
        let id = seed_page(
            app,
            uid,
            0,
            json!({"body": "x", "field_publish_on": future}),
        )
        .await;
        assert_eq!(timer_run_at(app, id, "publish").await, Some(future));

        let edit_path = format!("/item/{id}/edit");
        let html = get_page(app, &cookies, &edit_path).await;
        let body = format!(
            "{}&title=Schedule+probe&body=x&schedule_publish_on=",
            form_tokens(&html)
        );
        let (status, text) = post_form(app, &cookies, &edit_path, body).await;
        assert!(status.is_success(), "{status}: {text}");

        assert!(
            stored_fields(app, id)
                .await
                .get("field_publish_on")
                .is_none(),
            "clearing the input must remove the stored key"
        );
        assert_eq!(
            timer_run_at(app, id, "publish").await,
            None,
            "clearing the input must cancel the timer"
        );
    });
}

// =============================================================================
// Refusals
// =============================================================================

/// A past date is refused, with its message, and nothing is written.
#[test]
fn a_past_date_is_refused() {
    run(|app| async move {
        let (uid, cookies) = editor(app, "b326past", "UTC").await;
        let id = seed_page(app, uid, 0, json!({"body": "x"})).await;

        let edit_path = format!("/item/{id}/edit");
        let html = get_page(app, &cookies, &edit_path).await;
        let body = format!(
            "{}&title=Schedule+probe&body=x&schedule_publish_on=2020-01-01T00%3A00",
            form_tokens(&html)
        );
        let (status, text) = post_form(app, &cookies, &edit_path, body).await;
        assert!(
            !status.is_redirection() && status != StatusCode::OK,
            "{status}"
        );
        assert!(
            text.contains("future"),
            "the past date must be refused with its message: {text}"
        );
        assert!(
            stored_fields(app, id)
                .await
                .get("field_publish_on")
                .is_none()
        );
    });
}

/// "Unpublish on" before "Publish on" is refused.
#[test]
fn unpublish_before_publish_is_refused() {
    run(|app| async move {
        let (uid, cookies) = editor(app, "b326order", "UTC").await;
        let id = seed_page(app, uid, 0, json!({"body": "x"})).await;

        let edit_path = format!("/item/{id}/edit");
        let html = get_page(app, &cookies, &edit_path).await;
        let body = format!(
            "{}&title=Schedule+probe&body=x&schedule_publish_on=2030-01-01T00%3A00&\
             schedule_unpublish_on=2029-01-01T00%3A00",
            form_tokens(&html)
        );
        let (_status, text) = post_form(app, &cookies, &edit_path, body).await;
        assert!(
            text.contains("after publish on"),
            "the bad order must be refused with its message: {text}"
        );
        assert!(
            stored_fields(app, id)
                .await
                .get("field_publish_on")
                .is_none()
        );
    });
}

/// "Publish on" is refused on an item that is already published.
#[test]
fn publish_on_is_refused_on_an_already_published_item() {
    run(|app| async move {
        let (uid, cookies) = editor(app, "b326pub", "UTC").await;
        let id = seed_page(app, uid, 1, json!({"body": "x"})).await;

        let edit_path = format!("/item/{id}/edit");
        let html = get_page(app, &cookies, &edit_path).await;
        let body = format!(
            "{}&title=Schedule+probe&body=x&status=1&_status_rendered=1&\
             schedule_publish_on=2030-01-01T00%3A00",
            form_tokens(&html)
        );
        let (_status, text) = post_form(app, &cookies, &edit_path, body).await;
        assert!(
            text.contains("already published"),
            "scheduling a publish on an already published item must be refused: {text}"
        );
        assert!(
            stored_fields(app, id)
                .await
                .get("field_publish_on")
                .is_none()
        );
    });
}

/// A DST gap time (one that does not exist in the viewer's zone) is refused.
#[test]
fn a_dst_gap_time_is_refused() {
    run(|app| async move {
        let (uid, cookies) = editor(app, "b326gap", "America/New_York").await;
        let id = seed_page(app, uid, 0, json!({"body": "x"})).await;

        // 2030-03-10 is the second Sunday of March 2030: clocks in
        // America/New_York spring forward at 02:00, so 02:30 does not exist.
        let edit_path = format!("/item/{id}/edit");
        let html = get_page(app, &cookies, &edit_path).await;
        let body = format!(
            "{}&title=Schedule+probe&body=x&schedule_publish_on=2030-03-10T02%3A30",
            form_tokens(&html)
        );
        let (_status, text) = post_form(app, &cookies, &edit_path, body).await;
        assert!(
            text.contains("does not exist"),
            "a DST gap time must be refused with its own message: {text}"
        );
        assert!(
            stored_fields(app, id)
                .await
                .get("field_publish_on")
                .is_none()
        );
    });
}

/// A user without the permission keeps the item's stored schedule unchanged:
/// the group is not rendered, so the save cannot wipe it.
#[test]
fn a_save_without_the_permission_keeps_the_stored_schedule() {
    run(|app| async move {
        let (uid, _owner_cookies) = editor(app, "b326owner", "UTC").await;
        let future = chrono::Utc::now().timestamp() + 3600;
        let id = seed_page(
            app,
            uid,
            0,
            json!({"body": "x", "field_publish_on": future}),
        )
        .await;
        let (_uid2, cookies) = common::user_holding(
            app,
            "b326noperm",
            &["create page content", "edit any content"],
        )
        .await;

        let edit_path = format!("/item/{id}/edit");
        let html = get_page(app, &cookies, &edit_path).await;
        assert!(!html.contains("schedule_publish_on"));
        let body = format!(
            "{}&title=Changed+by+a+user+without+the+permission&body=x",
            form_tokens(&html)
        );
        let (status, text) = post_form(app, &cookies, &edit_path, body).await;
        assert!(status.is_success(), "{status}: {text}");

        assert_eq!(
            stored_fields(app, id).await["field_publish_on"],
            json!(future),
            "a save by a user without {PERM} must not wipe the stored schedule"
        );
        assert_eq!(timer_run_at(app, id, "publish").await, Some(future));
    });
}

// =============================================================================
// The JSON item API
// =============================================================================

/// The JSON item API refuses a past `field_publish_on` the same as the form:
/// the form is not the only guard.
#[test]
fn the_json_item_api_refuses_a_past_publish_on() {
    run(|app| async move {
        let (uid, cookies) = editor(app, "b326json", "UTC").await;
        let id = seed_page(app, uid, 0, json!({"body": "x"})).await;

        let edit_path = format!("/item/{id}/edit");
        let page = get_page(app, &cookies, &edit_path).await;
        let csrf = input_value(&page, "_token").unwrap();
        let (status, text) = post_json(
            app,
            &cookies,
            &edit_path,
            &csrf,
            json!({"title": "Schedule probe", "fields": {"body": "x", "field_publish_on": 1}}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert!(text.contains("future"), "{text}");
        assert!(
            stored_fields(app, id)
                .await
                .get("field_publish_on")
                .is_none()
        );
    });
}

/// The JSON item API refuses "already published" the same as the form.
#[test]
fn the_json_item_api_refuses_a_publish_on_an_already_published_item() {
    run(|app| async move {
        let (uid, cookies) = editor(app, "b326jsonpub", "UTC").await;
        let id = seed_page(app, uid, 1, json!({"body": "x"})).await;

        let edit_path = format!("/item/{id}/edit");
        let page = get_page(app, &cookies, &edit_path).await;
        let csrf = input_value(&page, "_token").unwrap();
        let far_future = chrono::Utc::now().timestamp() + 86_400;
        let (status, text) = post_json(
            app,
            &cookies,
            &edit_path,
            &csrf,
            json!({
                "title": "Schedule probe",
                "fields": {"body": "x", "field_publish_on": far_future},
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
        assert!(text.contains("already published"), "{text}");
    });
}

// =============================================================================
// End to end: the timer actually publishes
// =============================================================================

fn start_scheduler(
    timers: &std::sync::Arc<trovato_kernel::timer::TimerService>,
) -> (
    tokio_util::sync::CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    let token = tokio_util::sync::CancellationToken::new();
    let handle = tokio::spawn(timers.clone().run_scheduler(token.clone()));
    (token, handle)
}

async fn stop_scheduler(
    token: tokio_util::sync::CancellationToken,
    handle: tokio::task::JoinHandle<()>,
) {
    token.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(30), handle).await;
}

/// An item saved through the form with a publish date a few seconds out goes
/// live within a second of it, through the real timer, started here.
#[test]
fn an_item_scheduled_through_the_form_is_published_within_a_second() {
    run(|app| async move {
        let (_uid, cookies) = editor(app, "b326e2e", "UTC").await;
        let id = seed_page(app, _uid, 0, json!({"body": "x"})).await;

        let publish_at = chrono::Utc::now().timestamp() + 3;
        let local = chrono::DateTime::from_timestamp(publish_at, 0)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string();
        let edit_path = format!("/item/{id}/edit");
        let html = get_page(app, &cookies, &edit_path).await;
        let body = format!(
            "{}&title=Schedule+probe&body=x&schedule_publish_on={}",
            form_tokens(&html),
            urlencoding::encode(&local)
        );
        let (status, text) = post_form(app, &cookies, &edit_path, body).await;
        assert!(status.is_success(), "{status}: {text}");
        assert_eq!(timer_run_at(app, id, "publish").await, Some(publish_at));
        assert_eq!(item_status(app, id).await, 0);

        let timers = app.state.timers().clone();
        timers.set_owner_scope(Some(vec![PLUGIN.to_string()]));
        let (token, handle) = start_scheduler(&timers);
        let until = tokio::time::Instant::now() + Duration::from_secs(15);
        while item_status(app, id).await == 0 && tokio::time::Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let live_at = chrono::Utc::now();
        stop_scheduler(token, handle).await;

        assert_eq!(item_status(app, id).await, 1, "published by its timer");
        let late_ms = live_at.timestamp_millis() - publish_at * 1000;
        eprintln!("timer fired {late_ms} ms after the scheduled publish date");
        assert!(
            (0..1000).contains(&late_ms),
            "within a second: {late_ms} ms"
        );
    });
}
