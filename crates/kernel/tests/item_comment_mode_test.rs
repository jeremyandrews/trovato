#![allow(clippy::unwrap_used, clippy::expect_used)]
//! An item's own comment override (B239): `inherit` (the default, use the
//! content type's own `comment_mode`, B159), or `open`, `closed` or `hidden`
//! on that one item alone.
//!
//! Before this, comment settings lived only on the content type: an editor
//! could not close comments on a single post without closing them for every
//! post of that type, and a site migrated from Drupal 6 (whose `node.comment`
//! was per node) had nowhere to put the value it imported. `item.comment_mode`
//! is nullable; `NULL` is `inherit`, so every item that existed before this
//! migration keeps the behaviour it had.
//!
//! These drive the real router: the item page's thread, the comment write
//! routes (the JSON API and the reply page), the comment listing API, the
//! search vector, the item edit form and its permission.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, defer_cleanup, extract_cookies, run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// A fresh content type with `settings`, removed again when the test ends.
async fn content_type(app: &TestApp, settings: serde_json::Value) -> String {
    app.ensure_plugin_enabled("trovato_comments").await;
    let name = format!("b239_{}", &Uuid::now_v7().simple().to_string()[16..]);
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, $1, '', true, 'Title', 'core', $2)",
    )
    .bind(&name)
    .bind(&settings)
    .execute(&app.db)
    .await
    .expect("seed type");
    app.cleanup_content_type_on_exit(&name);
    let mut registered = settings;
    registered["fields"] = serde_json::json!([]);
    app.state
        .content_types()
        .replace(&name, &name, None, registered)
        .await
        .ok();
    name
}

/// A published item of `item_type`, deleted (with its comments) at the end.
async fn item(app: &TestApp, item_type: &str) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: item_type.to_string(),
                title: format!("B239 item {}", Uuid::now_v7().simple()),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: None,
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id;
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM comment WHERE item_id = $1")
            .bind(id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(format!("/item/{id}"))
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

/// A signed-in member who may post without approval, and their id.
async fn member(app: &TestApp) -> (String, Uuid) {
    let name = format!("b239m_{}", Uuid::now_v7().simple());
    let cookies = app
        .create_and_login_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(&name)
        .fetch_one(&app.db)
        .await
        .expect("find user");
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM comment WHERE author_id = $1")
            .bind(user_id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await;
    });
    common::grant_via_role(
        app,
        user_id,
        &["access content", "post comments", "skip comment approval"],
    )
    .await;
    app.state.permissions().invalidate_all();
    (cookies, user_id)
}

/// A published comment written at `created`, its body `body`.
async fn comment_at(app: &TestApp, item_id: Uuid, author: Uuid, created: i64, body: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO comment (id, item_id, parent_id, author_id, body, body_format, status, created, changed) \
         VALUES ($1, $2, NULL, $3, $4, 'plain_text', 1, $5, $5)",
    )
    .bind(id)
    .bind(item_id)
    .bind(author)
    .bind(body)
    .bind(created)
    .execute(&app.db)
    .await
    .expect("insert comment");
    id
}

async fn page(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("read body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Post a comment through the JSON route with a fresh CSRF token.
async fn post_comment(app: &TestApp, cookies: &str, item_id: Uuid) -> StatusCode {
    let response = app
        .request_with_cookies(Request::get("/").body(Body::empty()).unwrap(), cookies)
        .await;
    let fresh = extract_cookies(&response);
    let cookies = if fresh.is_empty() {
        cookies.to_string()
    } else {
        fresh
    };
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&bytes);
    let marker = "name=\"csrf-token\" content=\"";
    let start = html.find(marker).expect("csrf meta") + marker.len();
    let token = html[start..].split('"').next().unwrap().to_string();
    app.request_with_cookies(
        Request::post(format!("/api/v1/item/{item_id}/comments"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("X-CSRF-Token", token)
            .body(Body::from(r#"{"body":"A new remark"}"#))
            .unwrap(),
        &cookies,
    )
    .await
    .status()
}

async fn comment_mode_of(app: &TestApp, id: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT comment_mode FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .expect("read comment_mode")
}

/// An item set to `hidden` loses its comment area even though its type is
/// open; a sibling item of the same type keeps its thread and form.
#[test]
fn an_item_can_hide_its_own_comments_without_changing_its_type() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app, serde_json::json!({})).await;
        let (cookies, author) = member(app).await;

        let hidden = item(app, &item_type).await;
        comment_at(app, hidden, author, 1_700_000_000, "Said before hiding").await;
        app.state
            .items()
            .set_comment_mode(hidden, "hidden", &admin())
            .await
            .expect("set hidden")
            .expect("item exists");
        assert_eq!(
            comment_mode_of(app, hidden).await.as_deref(),
            Some("hidden")
        );

        let (_, html) = page(app, &cookies, &format!("/item/{hidden}")).await;
        assert!(!html.contains("id=\"comments\""), "no comment area at all");
        assert!(!html.contains("Said before hiding"));
        assert_eq!(
            post_comment(app, &cookies, hidden).await,
            StatusCode::FORBIDDEN
        );

        // The sibling, with no override, is still open.
        let open = item(app, &item_type).await;
        let (_, html) = page(app, &cookies, &format!("/item/{open}")).await;
        assert!(html.contains("data-comment-form"), "the sibling stays open");
        assert_eq!(post_comment(app, &cookies, open).await, StatusCode::OK);
    });
}

/// `closed` shows the existing thread with no form, same as the type setting,
/// but scoped to one item.
#[test]
fn an_item_can_close_its_own_comments_without_changing_its_type() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app, serde_json::json!({})).await;
        let (cookies, author) = member(app).await;

        let closed = item(app, &item_type).await;
        comment_at(app, closed, author, 1_700_000_000, "Said before closing").await;
        app.state
            .items()
            .set_comment_mode(closed, "closed", &admin())
            .await
            .expect("set closed")
            .expect("item exists");

        let (_, html) = page(app, &cookies, &format!("/item/{closed}")).await;
        assert!(html.contains("Said before closing"), "the thread is shown");
        assert!(!html.contains("data-comment-form"), "no form");
        assert_eq!(
            post_comment(app, &cookies, closed).await,
            StatusCode::FORBIDDEN
        );
        let (status, _) = page(app, &cookies, &format!("/comment/reply/{closed}")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "the reply page refuses too");
    });
}

/// The item's own override wins over its type's setting in either direction:
/// an item can open what its type closes, and close what its type leaves
/// open.
#[test]
fn an_items_override_wins_over_its_types_setting_either_direction() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, _) = member(app).await;

        let closed_type = content_type(app, serde_json::json!({ "comment_mode": "closed" })).await;
        let reopened = item(app, &closed_type).await;
        app.state
            .items()
            .set_comment_mode(reopened, "open", &admin())
            .await
            .expect("set open")
            .expect("item exists");
        let (_, html) = page(app, &cookies, &format!("/item/{reopened}")).await;
        assert!(
            html.contains("data-comment-form"),
            "the item's own open override must win over its closed type"
        );
        assert_eq!(post_comment(app, &cookies, reopened).await, StatusCode::OK);

        let open_type = content_type(app, serde_json::json!({})).await;
        let closed_one = item(app, &open_type).await;
        app.state
            .items()
            .set_comment_mode(closed_one, "closed", &admin())
            .await
            .expect("set closed")
            .expect("item exists");
        let (_, html) = page(app, &cookies, &format!("/item/{closed_one}")).await;
        assert!(
            !html.contains("data-comment-form"),
            "the item's own closed override must win over its open type"
        );
    });
}

/// The JSON listing API omits every comment, and reports an empty total, for
/// an item whose own override is hidden.
#[test]
fn listing_omits_comments_when_the_items_own_override_is_hidden() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app, serde_json::json!({})).await;
        let (_, author) = member(app).await;
        let id = item(app, &item_type).await;
        comment_at(app, id, author, 1_700_000_000, "Hidden from the listing").await;
        app.state
            .items()
            .set_comment_mode(id, "hidden", &admin())
            .await
            .expect("set hidden")
            .expect("item exists");

        let response = app
            .request_with_cookies(
                Request::get(format!("/api/v1/item/{id}/comments"))
                    .body(Body::empty())
                    .unwrap(),
                "",
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["total"], 0, "body was {json}");
        assert_eq!(json["data"].as_array().unwrap().len(), 0);
    });
}

/// A word that appears only in a comment on an item whose own override is
/// hidden must not be found by search; the same word on an open item is
/// found, so the test is about the override and not about search being
/// unable to see the comment at all.
#[test]
fn search_omits_comments_when_the_items_own_override_is_hidden() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app, serde_json::json!({})).await;
        let (_, author) = member(app).await;

        let hidden_id = item(app, &item_type).await;
        app.state
            .items()
            .set_comment_mode(hidden_id, "hidden", &admin())
            .await
            .expect("set hidden")
            .expect("item exists");
        let hidden_word = format!("zzhidword{}", Uuid::now_v7().simple());
        comment_at(
            app,
            hidden_id,
            author,
            1_700_000_000,
            &format!("A remark about {hidden_word}"),
        )
        .await;

        let open_id = item(app, &item_type).await;
        let open_word = format!("zzopenword{}", Uuid::now_v7().simple());
        comment_at(
            app,
            open_id,
            author,
            1_700_000_000,
            &format!("A remark about {open_word}"),
        )
        .await;

        let hidden_hits: Vec<Uuid> = app
            .state
            .search()
            .search_unrecorded(&hidden_word, &[LIVE_STAGE_ID], None, 50, 0)
            .await
            .expect("search")
            .results
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert!(
            hidden_hits.is_empty(),
            "a word only in a hidden item's comment must not find it: {hidden_hits:?}"
        );

        let open_hits: Vec<Uuid> = app
            .state
            .search()
            .search_unrecorded(&open_word, &[LIVE_STAGE_ID], None, 50, 0)
            .await
            .expect("search")
            .results
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(
            open_hits,
            vec![open_id],
            "the same comment on an open item must still be searchable"
        );
    });
}

/// The item form offers the comment override only to a holder of
/// `set item comment status`, and a save by that holder persists it and
/// changes the item's behaviour.
#[test]
fn the_item_form_offers_the_override_only_to_a_holder_of_the_permission_and_saves_it() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app, serde_json::json!({})).await;
        let id = item(app, &item_type).await;

        // A separate item for the permission check: the edit form's GET
        // takes the editor's content lock, and taking it on `id` here would
        // make the holder's save below conflict with it.
        let unprivileged_id = item(app, &item_type).await;
        let (_, editor_cookies) = common::user_holding(
            app,
            "b239editor",
            &["access administration pages", "edit any content"],
        )
        .await;
        let unprivileged_path = format!("/admin/content/{unprivileged_id}/edit");
        let (status, html) = page(app, &editor_cookies, &unprivileged_path).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !html.contains(r#"name="comment_override""#),
            "an editor without the permission must not be offered the control"
        );

        let path = format!("/admin/content/{id}/edit");
        let (_, holder_cookies) = common::user_holding(
            app,
            "b239holder",
            &[
                "access administration pages",
                "edit any content",
                "set item comment status",
            ],
        )
        .await;
        let (status, html) = page(app, &holder_cookies, &path).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains(r#"name="comment_override""#),
            "a holder of the permission must be offered the control"
        );

        let at = html.find(r#"name="_form_build_id""#).expect("a build id");
        let scoped = &html[html[..at].rfind("<form").unwrap()..];
        let token = {
            let needle = r#"name="_token""#;
            let at = scoped.find(needle).unwrap();
            let start = scoped[..at].rfind('<').unwrap();
            let end = at + scoped[at..].find('>').unwrap();
            let tag = &scoped[start..end];
            let vat = tag.find(r#"value=""#).unwrap() + 7;
            let vend = tag[vat..].find('"').unwrap() + vat;
            tag[vat..vend].to_string()
        };
        let build_id = {
            let needle = r#"name="_form_build_id""#;
            let at = scoped.find(needle).unwrap();
            let start = scoped[..at].rfind('<').unwrap();
            let end = at + scoped[at..].find('>').unwrap();
            let tag = &scoped[start..end];
            let vat = tag.find(r#"value=""#).unwrap() + 7;
            let vend = tag[vat..].find('"').unwrap() + vat;
            tag[vat..vend].to_string()
        };

        let body = format!(
            "_token={}&_form_build_id={}&title=B239+form+save&comment_override=hidden",
            urlencoding::encode(&token),
            urlencoding::encode(&build_id),
        );
        let response = app
            .request_with_cookies(
                Request::builder()
                    .method("POST")
                    .uri(&path)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &holder_cookies,
            )
            .await;
        let saved_status = response.status();
        let saved_body = {
            let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
                .await
                .unwrap();
            String::from_utf8_lossy(&bytes).into_owned()
        };
        assert!(
            saved_status.is_redirection(),
            "save refused: {saved_status} {saved_body}"
        );
        assert_eq!(comment_mode_of(app, id).await.as_deref(), Some("hidden"));
    });
}

/// `item.comment_mode` is `NULL` (`inherit`) for every item the normal write
/// path creates: the migration's own guarantee, that no existing item's
/// behaviour changes.
#[test]
fn every_item_is_inherit_unless_its_override_is_set() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app, serde_json::json!({})).await;
        let id = item(app, &item_type).await;
        assert_eq!(
            comment_mode_of(app, id).await,
            None,
            "a freshly created item must be inherit"
        );
    });
}
