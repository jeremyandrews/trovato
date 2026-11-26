#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Comment threads per content type (B159: K-comment-per-type,
//! K-comment-pager, K-comment-order).
//!
//! Every item of every type showed its whole thread and a comment form: a
//! 174 comment discussion was one page, a Basic page or a gallery image (types
//! whose comments the original hid) had a comment area, and the thread was
//! always threaded, oldest first. A content type now says whether its items'
//! comments are open, closed or hidden, threaded or flat, oldest or newest
//! first, and how many a page of the thread shows (50 unless it says).
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

const COMMENT: &str = "<article class=\"comment comment--depth-";

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// A fresh content type with `settings`, removed again when the test ends.
async fn content_type(app: &TestApp, settings: serde_json::Value) -> String {
    app.ensure_plugin_enabled("trovato_comments").await;
    let name = format!("b159_thread_{}", &Uuid::now_v7().simple().to_string()[16..]);
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
                title: format!("Thread {}", Uuid::now_v7().simple()),
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
    let name = format!("b159t_{}", Uuid::now_v7().simple());
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
async fn comment_at(
    app: &TestApp,
    item_id: Uuid,
    parent: Option<Uuid>,
    author: Uuid,
    created: i64,
    body: &str,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO comment (id, item_id, parent_id, author_id, body, body_format, status, created, changed) \
         VALUES ($1, $2, $3, $4, $5, 'plain_text', 1, $6, $6)",
    )
    .bind(id)
    .bind(item_id)
    .bind(parent)
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

/// The bodies of the comments on `html`, in page order.
fn bodies(html: &str) -> Vec<String> {
    html.split("<div class=\"comment__body\">")
        .skip(1)
        .map(|chunk| {
            let text = chunk.split("</div>").next().unwrap();
            let text = text.replace("<p>", "").replace("</p>", "");
            text.split("{#").next().unwrap().trim().to_string()
        })
        .collect()
}

/// The groups discussion: 174 comments on a type that says nothing is four
/// pages of 50, 50, 50 and 24, with a pager; a page past the end is the last.
#[test]
fn a_long_thread_is_paged_at_fifty() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app, serde_json::json!({})).await;
        let item_id = item(app, &item_type).await;
        let (_, author) = member(app).await;
        for n in 0..174 {
            comment_at(
                app,
                item_id,
                None,
                author,
                1_600_000_000 + n,
                &format!("Remark {n}"),
            )
            .await;
        }

        let (status, first) = page(app, "", &format!("/item/{item_id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first.matches(COMMENT).count(), 50);
        assert!(
            first.contains("Comments (174)"),
            "the heading counts the thread"
        );
        assert!(first.contains("pager__item"), "a pager");
        assert!(first.contains("?page=4"), "a link to the last page");
        assert_eq!(bodies(&first)[0], "Remark 0");

        let (_, second) = page(app, "", &format!("/item/{item_id}?page=2")).await;
        assert_eq!(second.matches(COMMENT).count(), 50);
        assert_eq!(bodies(&second)[0], "Remark 50");

        for last in ["4", "9"] {
            let (status, html) = page(app, "", &format!("/item/{item_id}?page={last}")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(html.matches(COMMENT).count(), 24, "page {last}");
            assert_eq!(bodies(&html)[23], "Remark 173");
        }

        let (status, html) = page(app, "", &format!("/item/{item_id}?page=abc")).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a junk page number is the first page"
        );
        assert_eq!(html.matches(COMMENT).count(), 50);
    });
}

/// A type's own count, and no pager when one page holds the thread.
#[test]
fn a_type_sets_its_own_page_size() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app, serde_json::json!({ "comment_per_page": 20 })).await;
        let item_id = item(app, &item_type).await;
        let (_, author) = member(app).await;
        for n in 0..25 {
            comment_at(
                app,
                item_id,
                None,
                author,
                1_600_000_000 + n,
                &format!("R{n}"),
            )
            .await;
        }
        let (_, html) = page(app, "", &format!("/item/{item_id}")).await;
        assert_eq!(html.matches(COMMENT).count(), 20);

        let short_type = content_type(app, serde_json::json!({})).await;
        let short = item(app, &short_type).await;
        comment_at(app, short, None, author, 1_600_000_000, "Alone").await;
        let (_, html) = page(app, "", &format!("/item/{short}")).await;
        assert!(!html.contains("pager__item"), "one page, no pager");
    });
}

/// Hidden: no comment area, and the comment route refuses a post. Closed:
/// the thread, no form, and the same refusal. Open: the form.
#[test]
fn a_type_opens_closes_or_hides_its_comments() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, author) = member(app).await;

        let hidden_type = content_type(app, serde_json::json!({ "comment_mode": "hidden" })).await;
        let hidden = item(app, &hidden_type).await;
        comment_at(
            app,
            hidden,
            None,
            author,
            1_600_000_000,
            "Imported long ago",
        )
        .await;
        let (_, html) = page(app, &cookies, &format!("/item/{hidden}")).await;
        assert!(!html.contains("id=\"comments\""), "no comment area");
        assert!(!html.contains("Imported long ago"));
        assert_eq!(
            post_comment(app, &cookies, hidden).await,
            StatusCode::FORBIDDEN
        );
        let (status, _) = page(app, &cookies, &format!("/comment/reply/{hidden}")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let closed_type = content_type(app, serde_json::json!({ "comment_mode": "closed" })).await;
        let closed = item(app, &closed_type).await;
        comment_at(
            app,
            closed,
            None,
            author,
            1_600_000_000,
            "Said before closing",
        )
        .await;
        let (_, html) = page(app, &cookies, &format!("/item/{closed}")).await;
        assert!(html.contains("Said before closing"), "the thread is shown");
        assert!(!html.contains("data-comment-form"), "no form");
        assert!(html.contains("Comments are closed."));
        assert_eq!(
            post_comment(app, &cookies, closed).await,
            StatusCode::FORBIDDEN
        );

        let open_type = content_type(app, serde_json::json!({ "comment_mode": "open" })).await;
        let open = item(app, &open_type).await;
        let (_, html) = page(app, &cookies, &format!("/item/{open}")).await;
        assert!(html.contains("data-comment-form"));
        assert_eq!(post_comment(app, &cookies, open).await, StatusCode::OK);
    });
}

/// Threaded newest first: the newest thread on top, each reply still under
/// the comment it answers. Flat: by date, no indentation.
#[test]
fn a_type_orders_its_thread() {
    run_test(async {
        let app = shared_app().await;
        let (_, author) = member(app).await;

        let fill = |item_id| async move {
            let first = comment_at(app, item_id, None, author, 1_600_000_000, "First thread").await;
            comment_at(app, item_id, None, author, 1_600_000_100, "Second thread").await;
            comment_at(
                app,
                item_id,
                Some(first),
                author,
                1_600_000_200,
                "Late reply to first",
            )
            .await;
        };

        let oldest_type = content_type(app, serde_json::json!({})).await;
        let oldest = item(app, &oldest_type).await;
        fill(oldest).await;
        let (_, html) = page(app, "", &format!("/item/{oldest}")).await;
        assert_eq!(
            bodies(&html),
            ["First thread", "Late reply to first", "Second thread"]
        );

        let newest_type = content_type(app, serde_json::json!({ "comment_order": "newest" })).await;
        let newest = item(app, &newest_type).await;
        fill(newest).await;
        let (_, html) = page(app, "", &format!("/item/{newest}")).await;
        assert_eq!(
            bodies(&html),
            ["Second thread", "First thread", "Late reply to first"],
            "newest thread first, the reply under its parent"
        );

        let flat_type = content_type(app, serde_json::json!({ "comment_threading": "flat" })).await;
        let flat = item(app, &flat_type).await;
        fill(flat).await;
        let (_, html) = page(app, "", &format!("/item/{flat}")).await;
        assert_eq!(
            bodies(&html),
            ["First thread", "Second thread", "Late reply to first"]
        );
        assert!(!html.contains("comment--depth-1"), "flat is not indented");
    });
}

/// The content type form edits the four settings.
#[test]
fn the_content_type_form_saves_comment_settings() {
    run_test(async {
        let app = shared_app().await;
        let item_type = content_type(app, serde_json::json!({})).await;
        let name = format!("b159a_{}", Uuid::now_v7().simple());
        let cookies = app
            .create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let pool = app.db.clone();
        let admin_name = name.clone();
        defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM users WHERE name = $1")
                .bind(admin_name)
                .execute(&pool)
                .await;
        });

        let edit = format!("/admin/structure/types/{item_type}/edit");
        let (status, form) = page(app, &cookies, &edit).await;
        assert_eq!(status, StatusCode::OK);
        assert!(form.contains("name=\"comment_mode\""));
        let field = |name: &str| {
            let marker = format!("name=\"{name}\" value=\"");
            let start = form.find(&marker).expect(name) + marker.len();
            form[start..].split('"').next().unwrap().to_string()
        };
        let body = format!(
            "_token={}&_form_build_id={}&label={item_type}&machine_name={item_type}&title_label=Title\
             &comment_mode=hidden&comment_threading=flat&comment_order=newest&comment_per_page=30",
            urlencoding::encode(&field("_token")),
            urlencoding::encode(&field("_form_build_id")),
        );
        let response = app
            .request_with_cookies(
                Request::post(&edit)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert!(response.status().is_redirection(), "{}", response.status());

        let settings: serde_json::Value =
            sqlx::query_scalar("SELECT settings FROM item_type WHERE type = $1")
                .bind(&item_type)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(settings["comment_mode"], "hidden");
        assert_eq!(settings["comment_threading"], "flat");
        assert_eq!(settings["comment_order"], "newest");
        assert_eq!(settings["comment_per_page"], 30);

        let (_, form) = page(app, &cookies, &edit).await;
        assert!(form.contains("<option value=\"hidden\" selected>"));
    });
}
