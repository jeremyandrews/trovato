//! Content locking end to end on the item edit form (`trovato_content_locking`).
//!
//! The plugin is on by default, and its two routes had no client: nothing ever
//! took a lock, so heartbeat and break had nothing to act on. The edit form now
//! takes the lock, shows another editor's, refuses a save under it, keeps its
//! own alive with the heartbeat, and lets a holder of `break content lock`
//! break it.
//!
//! The lease was 15 minutes, which only mattered once the form was gone — a
//! closed tab, a navigated-away tab or a crashed browser then blocked another
//! editor for up to 15 minutes. It is now 3 minutes (three missed
//! heartbeats), and `/api/v1/lock/release` lets a well-behaved exit (the
//! form's `pagehide` beacon) free the item at once instead of waiting out
//! even that.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use chrono::Utc;
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::{CreateItem, Item};
use uuid::Uuid;

/// The lease ([`trovato_kernel::services::content_lock`]'s
/// `DEFAULT_LOCK_DURATION_SECS`, private to that module): three missed
/// one-minute heartbeats.
const LEASE_SECS: i64 = 180;

/// The absolute lifetime cap (`MAX_ABSOLUTE_LOCK_LIFETIME_SECS`), unchanged by
/// this fix.
const MAX_LIFETIME_SECS: i64 = 86400;

async fn body_of(response: axum::response::Response) -> String {
    String::from_utf8_lossy(&to_bytes(response.into_body(), 4_000_000).await.unwrap()).to_string()
}

/// The edit form's page, as `cookies` sees it.
async fn edit_form(app: &TestApp, item_id: Uuid, cookies: &str) -> String {
    let response = app
        .request_with_cookies(
            Request::get(format!("/admin/content/{item_id}/edit"))
                .header("x-forwarded-for", common::test_ip_for(cookies))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    body_of(response).await
}

/// The page's CSRF token (`<meta name="csrf-token">`).
fn csrf_of(html: &str) -> String {
    let marker = "name=\"csrf-token\" content=\"";
    let start = html.find(marker).expect("a csrf meta tag") + marker.len();
    let end = html[start..].find('"').unwrap() + start;
    html[start..end].to_string()
}

async fn post_json(
    app: &TestApp,
    path: &str,
    cookies: &str,
    csrf: &str,
    body: serde_json::Value,
) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::post(path)
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-csrf-token", csrf)
                .header("x-forwarded-for", common::test_ip_for(cookies))
                .body(Body::from(body.to_string()))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_of(response).await)
}

/// A release the way `navigator.sendBeacon` sends it from `pagehide`: a
/// form-urlencoded body carrying the CSRF token in a `_token` field, no
/// `X-CSRF-Token` header (a beacon cannot set one).
async fn post_beacon_release(
    app: &TestApp,
    cookies: &str,
    csrf: &str,
    item_id: Uuid,
) -> (StatusCode, String) {
    let token: String = url::form_urlencoded::byte_serialize(csrf.as_bytes()).collect();
    let body = format!("entity_type=item&entity_id={item_id}&_token={token}");
    let response = app
        .request_with_cookies(
            Request::post("/api/v1/lock/release")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("x-forwarded-for", common::test_ip_for(cookies))
                .body(Body::from(body))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_of(response).await)
}

async fn holder(app: &TestApp, item_id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar(
        "SELECT user_id FROM editing_lock WHERE entity_type = 'item' AND entity_id = $1",
    )
    .bind(item_id.to_string())
    .fetch_optional(&app.db)
    .await
    .unwrap()
}

/// `(locked_at, expires_at)` of the lock on `item_id`, if any.
async fn lock_times(app: &TestApp, item_id: Uuid) -> Option<(i64, i64)> {
    sqlx::query_as(
        "SELECT locked_at, expires_at FROM editing_lock WHERE entity_type = 'item' AND entity_id = $1",
    )
    .bind(item_id.to_string())
    .fetch_optional(&app.db)
    .await
    .unwrap()
}

/// Back-date a lock's `expires_at`/`locked_at` directly in the database — the
/// test's "controllable clock", standing in for the minutes an abandoned form
/// or an un-renewed lock would otherwise take to actually elapse.
async fn backdate_lock(app: &TestApp, item_id: Uuid, locked_at: i64, expires_at: i64) {
    sqlx::query(
        "UPDATE editing_lock SET locked_at = $1, expires_at = $2 \
         WHERE entity_type = 'item' AND entity_id = $3",
    )
    .bind(locked_at)
    .bind(expires_at)
    .bind(item_id.to_string())
    .execute(&app.db)
    .await
    .unwrap();
}

/// A fresh conference item plus two logged-in admins, A and B, for a lock test.
async fn two_editors(app: &TestApp, label: &str) -> (Uuid, String, Uuid, String, Uuid) {
    app.ensure_plugin_enabled("trovato_content_locking").await;
    // The shared app's plugin directory does not discover real plugins (see
    // `common::ensure_plugin_tables`), so `editing_lock` otherwise exists only
    // by luck of shard order; apply its migration directly, idempotently.
    common::ensure_plugin_tables(app, "trovato_content_locking");
    app.ensure_conference_type().await;

    let a_name = common::username(&format!("{label}_a"));
    let b_name = common::username(&format!("{label}_b"));
    let a = app
        .create_and_login_admin(&a_name, "TestPassword123!", &format!("{a_name}@test.com"))
        .await;
    let b = app
        .create_and_login_admin(&b_name, "TestPassword123!", &format!("{b_name}@test.com"))
        .await;
    let a_id = common::user_id_of(app, &a_name).await;
    let b_id = common::user_id_of(app, &b_name).await;

    let item = Item::create(
        &app.db,
        CreateItem {
            item_type: "conference".to_string(),
            title: format!("lock probe {} {}", label, Uuid::now_v7().simple()),
            author_id: a_id,
            status: Some(1),
            promote: None,
            sticky: None,
            fields: None,
            stage_id: None,
            language: None,
            log: None,
        },
    )
    .await
    .expect("create item");
    let pool = app.db.clone();
    let item_id = item.id;
    common::defer_cleanup(move || async move {
        let _ = Item::delete(&pool, item_id).await;
    });

    (item_id, a, a_id, b, b_id)
}

#[test]
fn the_edit_form_takes_shows_keeps_and_breaks_the_lock() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled("trovato_content_locking").await;
        common::ensure_plugin_tables(app, "trovato_content_locking");
        app.ensure_conference_type().await;

        let a_name = common::username("lock_a");
        let b_name = common::username("lock_b");
        let a = app
            .create_and_login_admin(&a_name, "TestPassword123!", &format!("{a_name}@test.com"))
            .await;
        let b = app
            .create_and_login_admin(&b_name, "TestPassword123!", &format!("{b_name}@test.com"))
            .await;
        let a_id = common::user_id_of(app, &a_name).await;
        let b_id = common::user_id_of(app, &b_name).await;

        let item = Item::create(
            &app.db,
            CreateItem {
                item_type: "conference".to_string(),
                title: format!("lock probe {}", Uuid::now_v7().simple()),
                author_id: a_id,
                status: Some(1),
                promote: None,
                sticky: None,
                fields: None,
                stage_id: None,
                language: None,
                log: None,
            },
        )
        .await
        .expect("create item");
        let pool = app.db.clone();
        let item_id = item.id;
        common::defer_cleanup(move || async move {
            let _ = Item::delete(&pool, item_id).await;
        });
        let lock_body = serde_json::json!({"entity_type": "item", "entity_id": item_id});

        // A opens the form: A holds the lock, and the page keeps it alive.
        let a_page = edit_form(app, item_id, &a).await;
        assert_eq!(holder(app, item_id).await, Some(a_id));
        assert!(a_page.contains("data-content-lock "), "the heartbeat hook");
        assert!(!a_page.contains("is editing this item"));

        // B opens it: told A is editing, offered the break.
        let b_page = edit_form(app, item_id, &b).await;
        assert!(b_page.contains("is editing this item"), "{b_page}");
        assert!(b_page.contains(&a_name));
        assert!(b_page.contains("data-content-lock-break"));
        assert_eq!(holder(app, item_id).await, Some(a_id), "B did not take it");

        // B's save under A's lock is refused.
        let (status, body) = post_json(
            app,
            &format!("/item/{item_id}/edit"),
            &b,
            &csrf_of(&b_page),
            serde_json::json!({"title": "B was here"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");

        // A's heartbeat extends it, and each answer carries the next token.
        let (status, body) = post_json(
            app,
            "/api/v1/lock/heartbeat",
            &a,
            &csrf_of(&a_page),
            lock_body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let next: serde_json::Value = serde_json::from_str(&body).unwrap();
        let token = next["csrf_token"]
            .as_str()
            .expect("a fresh token")
            .to_string();
        let (status, body) =
            post_json(app, "/api/v1/lock/heartbeat", &a, &token, lock_body.clone()).await;
        assert_eq!(status, StatusCode::OK, "the returned token works: {body}");

        // B breaks it, and the form is B's.
        let b_page = edit_form(app, item_id, &b).await;
        let (status, body) = post_json(
            app,
            "/api/v1/lock/break",
            &b,
            &csrf_of(&b_page),
            lock_body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let b_page = edit_form(app, item_id, &b).await;
        assert!(!b_page.contains("is editing this item"));
        assert_eq!(holder(app, item_id).await, Some(b_id));

        // Now A's save is the one refused, and B's goes through and lets go.
        let a_page = edit_form(app, item_id, &a).await;
        assert!(a_page.contains("is editing this item"));
        let (status, body) = post_json(
            app,
            &format!("/item/{item_id}/edit"),
            &a,
            &csrf_of(&a_page),
            serde_json::json!({"title": "A was here"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let (status, body) = post_json(
            app,
            &format!("/item/{item_id}/edit"),
            &b,
            &csrf_of(&b_page),
            serde_json::json!({"title": "B saved"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(holder(app, item_id).await, None, "a save releases the lock");
    });
}

/// Acquiring a lock sets a lease of [`LEASE_SECS`], not the old 15 minutes.
#[test]
fn the_lease_is_3_minutes() {
    run_test(async {
        let app = shared_app().await;
        let (item_id, a, a_id, _b, _b_id) = two_editors(app, "lease").await;

        let _ = edit_form(app, item_id, &a).await;
        assert_eq!(holder(app, item_id).await, Some(a_id));
        let (locked_at, expires_at) = lock_times(app, item_id).await.expect("a lock row");
        assert_eq!(
            expires_at - locked_at,
            LEASE_SECS,
            "the lease is {LEASE_SECS}s, not the old 900s"
        );
    });
}

/// A lock whose holder stopped heartbeating is free to another editor once the
/// lease has passed — simulated here by back-dating the row rather than
/// sleeping out 3 real minutes.
#[test]
fn an_abandoned_lock_frees_after_the_lease() {
    run_test(async {
        let app = shared_app().await;
        let (item_id, a, a_id, b, b_id) = two_editors(app, "abandoned").await;

        let _ = edit_form(app, item_id, &a).await;
        assert_eq!(holder(app, item_id).await, Some(a_id));

        // A's browser died; no further heartbeat ever arrives. Put the lock
        // where it would be LEASE_SECS + 1 after the last one.
        let now = Utc::now().timestamp();
        backdate_lock(app, item_id, now - LEASE_SECS - 1, now - 1).await;

        // B opens the form seconds later and gets it, not "is editing this item".
        let b_page = edit_form(app, item_id, &b).await;
        assert!(
            !b_page.contains("is editing this item"),
            "the lapsed lock should have been free: {b_page}"
        );
        assert_eq!(holder(app, item_id).await, Some(b_id));
    });
}

/// A release from the holder frees the lock at once, before the lease would
/// otherwise have lapsed.
#[test]
fn release_frees_the_lock_at_once() {
    run_test(async {
        let app = shared_app().await;
        let (item_id, a, a_id, _b, _b_id) = two_editors(app, "release_holder").await;

        let a_page = edit_form(app, item_id, &a).await;
        assert_eq!(holder(app, item_id).await, Some(a_id));

        let (status, body) = post_json(
            app,
            "/api/v1/lock/release",
            &a,
            &csrf_of(&a_page),
            serde_json::json!({"entity_type": "item", "entity_id": item_id}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            holder(app, item_id).await,
            None,
            "the holder's release should free the item immediately"
        );
    });
}

/// A release from someone other than the holder changes nothing: the lock the
/// route acts on is always the caller's own, so it cannot free another
/// editor's lock.
#[test]
fn release_by_another_user_changes_nothing() {
    run_test(async {
        let app = shared_app().await;
        let (item_id, a, a_id, b, _b_id) = two_editors(app, "release_other").await;

        let _ = edit_form(app, item_id, &a).await;
        assert_eq!(holder(app, item_id).await, Some(a_id));

        // B is logged in but holds no lock on this item; B's own release is a
        // no-op, and must not touch A's lock.
        let b_page = edit_form(app, item_id, &b).await;
        let (status, body) = post_json(
            app,
            "/api/v1/lock/release",
            &b,
            &csrf_of(&b_page),
            serde_json::json!({"entity_type": "item", "entity_id": item_id}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            holder(app, item_id).await,
            Some(a_id),
            "B's release must not free A's lock"
        );
    });
}

/// The release route also accepts the body shape `navigator.sendBeacon`
/// sends on `pagehide`: form-urlencoded, token in a `_token` field, no
/// `X-CSRF-Token` header (a beacon cannot set one).
#[test]
fn release_accepts_a_beacon_shaped_request() {
    run_test(async {
        let app = shared_app().await;
        let (item_id, a, a_id, _b, _b_id) = two_editors(app, "release_beacon").await;

        let a_page = edit_form(app, item_id, &a).await;
        assert_eq!(holder(app, item_id).await, Some(a_id));

        let (status, body) = post_beacon_release(app, &a, &csrf_of(&a_page), item_id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            holder(app, item_id).await,
            None,
            "a beacon-shaped release should free the item"
        );
    });
}

/// The absolute lifetime cap still holds: a lock whose original `locked_at` is
/// past the cap cannot be extended by a heartbeat, even though it is still
/// within its own lease.
#[test]
fn absolute_lifetime_cap_still_holds() {
    run_test(async {
        let app = shared_app().await;
        let (item_id, a, a_id, _b, _b_id) = two_editors(app, "lifetime_cap").await;

        let a_page = edit_form(app, item_id, &a).await;
        assert_eq!(holder(app, item_id).await, Some(a_id));

        // Locked_at long enough ago that one more heartbeat would cross
        // MAX_LIFETIME_SECS; expires_at is still in the future, so this is not
        // just the lapsed-lease case.
        let now = Utc::now().timestamp();
        backdate_lock(app, item_id, now - MAX_LIFETIME_SECS - 1, now + 60).await;

        let (status, body) = post_json(
            app,
            "/api/v1/lock/heartbeat",
            &a,
            &csrf_of(&a_page),
            serde_json::json!({"entity_type": "item", "entity_id": item_id}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "the cap should refuse this heartbeat: {body}"
        );
    });
}
