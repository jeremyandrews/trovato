#![allow(clippy::unwrap_used, clippy::expect_used)]
//! One refusal rule, on every route.
//!
//! A JSON caller who is nobody gets `401`; a signed-in JSON caller who may not
//! do the thing gets `403`; both bodies are `AppError` (`code`, `message`,
//! `request_id`). An HTML caller who is nobody is sent to the login form with
//! a `303`, and a signed-in one who may not gets `403`. Before this, the answer
//! depended on which handler was asked: the JSON helpers said `403` to the
//! anonymous, hand-rolled session reads said `401`, the translation screens said
//! `400`, and the lock routes answered in plain text.
//!
//! The table below is one row per route family, each asked twice: by nobody and
//! by a signed-in user holding no permissions.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for, user_holding};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// What kind of caller a route serves, and so which half of the rule applies.
#[derive(Clone, Copy, Debug)]
enum Kind {
    /// A script: `401` for nobody, `403` for the unpermitted, `AppError` body.
    Json,
    /// A browser: `303` to `/user/login` for nobody, `403` for the unpermitted.
    Html,
    /// A JSON route anyone signed in may use: only the anonymous row applies.
    JsonLoginOnly,
    /// An HTML route anyone signed in may use: only the anonymous row applies.
    HtmlLoginOnly,
}

struct Row {
    method: &'static str,
    path: &'static str,
    content_type: Option<&'static str>,
    body: &'static str,
    kind: Kind,
}

const fn row(method: &'static str, path: &'static str, kind: Kind) -> Row {
    Row {
        method,
        path,
        content_type: None,
        body: "",
        kind,
    }
}

const fn json(method: &'static str, path: &'static str, body: &'static str, kind: Kind) -> Row {
    Row {
        method,
        path,
        content_type: Some("application/json"),
        body,
        kind,
    }
}

const LOCK: &str = r#"{"entity_type":"item","entity_id":"x"}"#;

/// The routes, one per family. `{item}` is replaced with a real item id.
const ROUTES: &[Row] = &[
    // The JSON permission helpers.
    row("GET", "/admin/queue/dlq", Kind::Json),
    // The admin AJAX callback's own trigger.
    json(
        "POST",
        "/system/ajax",
        r#"{"form_build_id":"x","trigger":"add_field","values":{}}"#,
        Kind::Json,
    ),
    // Formerly hand-rolled session reads.
    json("POST", "/api/lock/break", LOCK, Kind::Json),
    json("POST", "/api/lock/heartbeat", LOCK, Kind::JsonLoginOnly),
    row("GET", "/api/tokens", Kind::JsonLoginOnly),
    row("GET", "/api/v1/user/export", Kind::JsonLoginOnly),
    row("GET", "/api/v1/media/browse", Kind::JsonLoginOnly),
    json(
        "POST",
        "/api/block-editor/preview",
        r#"{"blocks":[]}"#,
        Kind::JsonLoginOnly,
    ),
    json(
        "POST",
        "/api/item/{item}/comments",
        r#"{"body":"x"}"#,
        Kind::JsonLoginOnly,
    ),
    json(
        "PUT",
        "/api/comment/00000000-0000-0000-0000-000000000001",
        r#"{"body":"x"}"#,
        Kind::JsonLoginOnly,
    ),
    // Item writes, decided from the loaded viewer.
    json("POST", "/item/add/page", r#"{"title":"x"}"#, Kind::Json),
    // HTML screens.
    row("GET", "/admin/content/{item}/translate", Kind::Html),
    row("GET", "/admin/structure/types", Kind::Html),
    row("GET", "/user/tokens", Kind::HtmlLoginOnly),
];

/// A published page, removed again when the test ends.
async fn published_item(app: &TestApp) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "page".to_string(),
                title: format!("refusal rule {}", Uuid::now_v7().simple()),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("refusal rule test".to_string()),
            },
            &UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]),
        )
        .await
        .expect("create item")
        .id;
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&db)
            .await;
    });
    id
}

async fn send(
    app: &TestApp,
    row: &Row,
    item: Uuid,
    cookies: &str,
    bucket: &str,
) -> (StatusCode, Option<String>, String) {
    let path = row.path.replace("{item}", &item.to_string());
    let mut builder = Request::builder()
        .method(row.method)
        .uri(&path)
        .header("x-forwarded-for", test_ip_for(bucket));
    if let Some(content_type) = row.content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    let response = app
        .request_with_cookies(builder.body(Body::from(row.body)).unwrap(), cookies)
        .await;
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        location,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn assert_app_error(body: &str, what: &str) {
    let value: serde_json::Value =
        serde_json::from_str(body).unwrap_or_else(|_| panic!("{what}: body is not JSON: {body}"));
    for key in ["code", "message", "request_id"] {
        assert!(
            value.get(key).and_then(|v| v.as_str()).is_some(),
            "{what}: AppError body has no `{key}`: {body}"
        );
    }
}

#[test]
fn every_route_refuses_by_one_rule() {
    run_test(async {
        let app = shared_app().await;
        for plugin in [
            "trovato_content_locking",
            "trovato_content_translation",
            "trovato_comments",
            "trovato_block_editor",
        ] {
            app.ensure_plugin_enabled(plugin).await;
        }
        common::ensure_translation_table(app);
        let item = published_item(app).await;
        let (_, cookies) = user_holding(app, "refusal", &[]).await;

        for row in ROUTES {
            let what = format!("{} {}", row.method, row.path);

            // Nobody.
            let (status, location, body) = send(app, row, item, "", "refusal-anon").await;
            match row.kind {
                Kind::Json | Kind::JsonLoginOnly => {
                    assert_eq!(status, StatusCode::UNAUTHORIZED, "{what} anonymous: {body}");
                    assert_app_error(&body, &format!("{what} anonymous"));
                }
                Kind::Html | Kind::HtmlLoginOnly => {
                    assert_eq!(status, StatusCode::SEE_OTHER, "{what} anonymous: {body}");
                    assert_eq!(location.as_deref(), Some("/user/login"), "{what} anonymous");
                }
            }

            // Signed in, holding nothing.
            let (status, _, body) = send(app, row, item, &cookies, "refusal-user").await;
            match row.kind {
                Kind::Json => {
                    assert_eq!(status, StatusCode::FORBIDDEN, "{what} unpermitted: {body}");
                    assert_app_error(&body, &format!("{what} unpermitted"));
                }
                Kind::Html => {
                    assert_eq!(status, StatusCode::FORBIDDEN, "{what} unpermitted: {body}");
                }
                Kind::JsonLoginOnly | Kind::HtmlLoginOnly => {
                    assert_ne!(status, StatusCode::UNAUTHORIZED, "{what} signed in: {body}");
                    assert!(!status.is_redirection(), "{what} signed in: {status}");
                }
            }
        }
    });
}

/// A session naming a user who has since been deleted is the anonymous
/// visitor, with the anonymous role's permissions, not a viewer with none.
#[test]
fn a_deleted_users_session_reads_as_anonymous() {
    run_test(async {
        let app = shared_app().await;
        let item = published_item(app).await;
        let (user_id, cookies) = user_holding(app, "refusal-gone", &[]).await;
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&app.db)
            .await
            .expect("delete the user under their own session");

        let view = row("GET", "/item/{item}", Kind::Html);
        let (anonymous, _, _) = send(app, &view, item, "", "refusal-gone-anon").await;
        assert_eq!(
            anonymous,
            StatusCode::OK,
            "an anonymous visitor reads a published page"
        );
        let (deleted, _, body) = send(app, &view, item, &cookies, "refusal-gone-user").await;
        assert_eq!(
            deleted,
            StatusCode::OK,
            "a deleted user's session reads what the anonymous role may: {body}"
        );

        // And a guard still treats them as nobody.
        let tokens = row("GET", "/api/tokens", Kind::JsonLoginOnly);
        let (status, _, _) = send(app, &tokens, item, &cookies, "refusal-gone-user").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    });
}
