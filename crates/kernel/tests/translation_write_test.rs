#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Something can write a content translation.
//!
//! `item_translation` has been read since it was added: the request overlay in
//! `routes/helpers.rs`, the join in the gather query builder, the sitemap's
//! alternate links and the translated menu labels all consult it. Nothing in the
//! kernel ever put a row in. Outside tests the only writer was SQL, so every one
//! of those readers was reading something no part of the product could produce,
//! and the admin translation screens could show a translation they had no way to
//! create.
//!
//! Two write paths are covered here: the admin form, and `config import`. The
//! third asked for, an SDK-visible one, is deliberately absent — see the note at
//! the bottom of this file.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

async fn create_item(app: &TestApp, title: &str) -> Uuid {
    app.state
        .items()
        .create(
            CreateItem {
                item_type: "page".to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({"body": "the original body"})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("translation write test".to_string()),
            },
            &UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]),
        )
        .await
        .expect("create item")
        .id
}

/// An admin session with the translation plugin enabled, and an untranslated item.
async fn fixture(app: &TestApp) -> (String, Uuid, String) {
    common::ensure_translation_table(app);
    app.ensure_plugin_enabled("trovato_content_translation")
        .await;

    let tag = Uuid::now_v7().simple().to_string();
    let item = create_item(app, &format!("Original {tag}")).await;

    let name = format!("transwrite_{tag}");
    let cookies = app
        .create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    (cookies, item, tag)
}

async fn get(app: &TestApp, cookies: &str, path: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(path)
                .header("x-forwarded-for", test_ip_for("transwrite"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn post(app: &TestApp, cookies: &str, path: &str, body: String) -> StatusCode {
    app.request_with_cookies(
        Request::post(path)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("x-forwarded-for", test_ip_for("transwrite"))
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

/// Pull a CSRF token out of a rendered page.
///
/// Tokens are pooled per session and single-use, so each POST needs its own GET.
async fn csrf_token(app: &TestApp, cookies: &str, path: &str) -> String {
    let (status, html) = get(app, cookies, path).await;
    assert_eq!(status, StatusCode::OK, "cannot read a token from {path}");
    let marker = r#"name="_token" value=""#;
    let start = html
        .find(marker)
        .map(|p| p + marker.len())
        .unwrap_or_else(|| panic!("no CSRF token on {path}"));
    let end = start + html[start..].find('"').expect("unterminated token");
    html[start..end].to_string()
}

async fn stored(app: &TestApp, item: Uuid, lang: &str) -> Option<(String, serde_json::Value)> {
    sqlx::query_as::<_, (String, serde_json::Value)>(
        "SELECT title, fields FROM item_translation WHERE item_id = $1 AND language = $2",
    )
    .bind(item)
    .bind(lang)
    .fetch_optional(&app.db)
    .await
    .expect("read back the translation")
}

/// The headline case: the admin form creates a translation that was not there.
#[test]
fn the_admin_form_writes_a_translation() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, item, tag) = fixture(app).await;
        let path = format!("/admin/content/{item}/translate/it");

        assert!(
            stored(app, item, "it").await.is_none(),
            "the fixture item starts untranslated"
        );

        let token = csrf_token(app, &cookies, &path).await;
        let status = post(
            app,
            &cookies,
            &path,
            format!("_token={token}&_form_build_id=x&title=Tradotto+{tag}&body=il+corpo"),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::SEE_OTHER,
            "a successful save redirects, like every other admin form"
        );

        let (title, fields) = stored(app, item, "it").await.expect("a row must exist now");
        assert_eq!(title, format!("Tradotto {tag}"));
        assert_eq!(
            fields.get("body").and_then(|v| v.as_str()),
            Some("il corpo"),
            "the translated field is stored, got {fields}"
        );
    });
}

/// Saving twice replaces, rather than accumulating a second row or merging.
#[test]
fn a_second_save_replaces_the_translation() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, item, _tag) = fixture(app).await;
        let path = format!("/admin/content/{item}/translate/it");

        let token = csrf_token(app, &cookies, &path).await;
        post(
            app,
            &cookies,
            &path,
            format!("_token={token}&_form_build_id=x&title=Primo&body=prima"),
        )
        .await;

        let token = csrf_token(app, &cookies, &path).await;
        post(
            app,
            &cookies,
            &path,
            format!("_token={token}&_form_build_id=x&title=Secondo&body=seconda"),
        )
        .await;

        let rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM item_translation WHERE item_id = $1 AND language = 'it'",
        )
        .bind(item)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(rows, 1, "the primary key is (item_id, language)");

        let (title, fields) = stored(app, item, "it").await.expect("row");
        assert_eq!(title, "Secondo");
        assert_eq!(fields.get("body").and_then(|v| v.as_str()), Some("seconda"));
    });
}

/// No `_token`, no write. The same protection the rest of the admin has.
#[test]
fn a_save_without_a_valid_csrf_token_writes_nothing() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, item, _tag) = fixture(app).await;
        let path = format!("/admin/content/{item}/translate/it");

        let status = post(
            app,
            &cookies,
            &path,
            "_token=not-a-real-token&_form_build_id=x&title=Iniettato&body=x".to_string(),
        )
        .await;
        assert_ne!(
            status,
            StatusCode::SEE_OTHER,
            "a forged token must not save"
        );
        assert!(
            stored(app, item, "it").await.is_none(),
            "and must leave no row behind"
        );
    });
}

/// `translate content` is the permission, and not holding it is a refusal.
#[test]
fn a_user_without_the_permission_cannot_write_a_translation() {
    run_test(async {
        let app = shared_app().await;
        let (admin_cookies, item, tag) = fixture(app).await;
        let path = format!("/admin/content/{item}/translate/it");

        // A real token, taken by an administrator who may use this screen, then
        // replayed by someone who may not: the permission check is what stops
        // it, not the absence of a token.
        let token = csrf_token(app, &admin_cookies, &path).await;

        let name = format!("transnoperm_{tag}");
        let cookies = app
            .create_and_login_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;

        let status = post(
            app,
            &cookies,
            &path,
            format!("_token={token}&_form_build_id=x&title=Vietato&body=x"),
        )
        .await;
        assert_ne!(status, StatusCode::SEE_OTHER);
        assert!(
            stored(app, item, "it").await.is_none(),
            "a user without `translate content` must write nothing"
        );
    });
}

/// A language the site does not have is a 404, not a stored row.
#[test]
fn a_save_in_an_unknown_language_is_refused() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, item, _tag) = fixture(app).await;

        let token = csrf_token(
            app,
            &cookies,
            &format!("/admin/content/{item}/translate/it"),
        )
        .await;
        let status = post(
            app,
            &cookies,
            &format!("/admin/content/{item}/translate/zz"),
            format!("_token={token}&_form_build_id=x&title=Nope&body=x"),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "an unknown language is a 404"
        );
        let rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM item_translation WHERE item_id = $1 AND language = 'zz'",
        )
        .bind(item)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(rows, 0);
    });
}

/// Translating an item into its own language is refused.
///
/// The overlay applies a translation on top of the original, so a row in the
/// item's own language is at best a no-op and at worst a second, diverging copy
/// of the same text with nothing to reconcile them.
#[test]
fn an_item_cannot_be_translated_into_its_own_language() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, item, _tag) = fixture(app).await;

        let token = csrf_token(
            app,
            &cookies,
            &format!("/admin/content/{item}/translate/it"),
        )
        .await;
        let status = post(
            app,
            &cookies,
            &format!("/admin/content/{item}/translate/en"),
            format!("_token={token}&_form_build_id=x&title=Same&body=x"),
        )
        .await;

        assert_ne!(status, StatusCode::SEE_OTHER);
        assert!(stored(app, item, "en").await.is_none());
    });
}

/// A translation can be taken back, not only added.
#[test]
fn a_translation_can_be_removed() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, item, _tag) = fixture(app).await;
        let path = format!("/admin/content/{item}/translate/it");

        let token = csrf_token(app, &cookies, &path).await;
        post(
            app,
            &cookies,
            &path,
            format!("_token={token}&_form_build_id=x&title=Da+rimuovere&body=x"),
        )
        .await;
        assert!(stored(app, item, "it").await.is_some());

        let token = csrf_token(app, &cookies, &path).await;
        let status = post(
            app,
            &cookies,
            &format!("{path}/delete"),
            format!("_token={token}"),
        )
        .await;

        assert_eq!(status, StatusCode::SEE_OTHER);
        assert!(
            stored(app, item, "it").await.is_none(),
            "the row must be gone"
        );
    });
}

/// A written translation is the one the overlay serves.
///
/// The point of writing one at all. Without this the test suite would prove a
/// row can be stored and never that anything reads it back, which is the half
/// that was already true.
#[test]
fn a_written_translation_reaches_the_reader() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, item, tag) = fixture(app).await;
        let path = format!("/admin/content/{item}/translate/it");

        let token = csrf_token(app, &cookies, &path).await;
        post(
            app,
            &cookies,
            &path,
            format!("_token={token}&_form_build_id=x&title=Tradotto+{tag}&body=il+corpo"),
        )
        .await;

        // The language negotiator reads Accept-Language; a path prefix only
        // reaches aliased paths, so this is the reliable way to ask in Italian.
        let response = app
            .request(
                Request::get(format!("/item/{item}"))
                    .header("accept-language", "it")
                    .header("x-forwarded-for", test_ip_for("transwrite-read"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("read body");
        let html = String::from_utf8_lossy(&bytes).into_owned();

        assert!(
            html.contains(&format!("Tradotto {tag}")),
            "the translated title must be what an Italian reader sees, got: {html}"
        );
    });
}

// # The third write path, and why it is not here
//
// The prompt asked for an SDK-visible way for a plugin to write a translation
// **only if the WIT already exposes an item write path that can carry one**. It
// does not, so none was added and this is the record of that.
//
// `item-api` offers `save-item(item-json) -> result<string, string>`, and the
// host behind it cannot express a translation in either direction:
//
// - the update branch builds `UpdateItem`, which has no `language` field at all,
//   so a `language` key in the plugin's JSON is dropped without comment;
// - the create branch builds `CreateItem` with `language: None` hardcoded, so a
//   plugin cannot set even an item's *own* language, let alone a translation of
//   it.
//
// The `tap-item-*` exports are notification hooks, not write calls, and carry
// the same language-free item JSON. The only WIT-reachable route to the table is
// the generic `db.insert("item_translation", ...)`, which is confined to a
// plugin's own migration-created tables and skips cache invalidation, search and
// revisions — `trovato_content_translation` created the table but declares
// `host_interfaces = []`, so it cannot even import `db`.
//
// Adding a carrier would mean changing a WIT function signature or an SDK type,
// which this work is fenced against. Recorded as a finding instead.
