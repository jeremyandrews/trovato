#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Promote to front page (B158, K-promote).
//!
//! Items had promote and sticky flags and the item forms set them, but the
//! content list could not promote or demote a selection, and a new item was
//! never promoted unless its author ticked the box: a content type had no
//! default, where Drupal's Article and Blog types promote by default and the
//! front page and feed list what is promoted. The content list now has the
//! four bulk actions, and a type's `promote_default` and `sticky_default`
//! settings decide a new item's flags when the write says nothing.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for};
use trovato_kernel::content::PUBLISH_CONTENT;
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// A type with no defaults, for the bulk actions.
const PLAIN: &str = "b158_promote_plain";
/// A type whose form sets the defaults on.
const PROMOTED: &str = "b158_promote_default";

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

async fn ensure_type(app: &TestApp, name: &str) {
    let settings = serde_json::json!({ "fields": [] });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, $1, '', true, 'Title', 'b158_test', $2) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(name)
    .bind(&settings)
    .execute(&app.db)
    .await
    .unwrap();
    app.state
        .content_types()
        .replace(name, name, None, settings)
        .await
        .ok();
}

fn token_in(html: &str) -> String {
    const NEEDLE: &str = r#"name="_token" value=""#;
    let at = html.find(NEEDLE).expect("a form token") + NEEDLE.len();
    let end = html[at..].find('"').unwrap() + at;
    html[at..end].to_string()
}

async fn get(app: &TestApp, cookies: &str, uri: &str, ip: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", ip)
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn post_form(app: &TestApp, cookies: &str, uri: &str, body: String, ip: &str) -> StatusCode {
    app.request_with_cookies(
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header("x-forwarded-for", ip)
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

fn service_admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec![])
}

fn forget(app: &TestApp, id: Uuid) {
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
}

async fn create(app: &TestApp, item_type: &str, user: &UserContext) -> Uuid {
    let item = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: item_type.to_string(),
                title: "Promote probe".to_string(),
                author_id: user.id,
                status: None,
                promote: None,
                sticky: None,
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: None,
            },
            user,
        )
        .await
        .expect("create");
    forget(app, item.id);
    item.id
}

async fn flags(app: &TestApp, id: Uuid) -> (i16, i16) {
    sqlx::query_as("SELECT promote, sticky FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

async fn bulk(app: &TestApp, cookies: &str, ip: &str, action: &str, id: Uuid) -> StatusCode {
    let (_, page) = get(app, cookies, "/admin/content", ip).await;
    let body = format!(
        "_token={}&action={action}&ids%5B%5D={id}",
        urlencoding::encode(&token_in(&page))
    );
    post_form(app, cookies, "/admin/content/bulk", body, ip).await
}

/// **The finding.** The content list promotes, demotes, and makes sticky
/// and not sticky, for an editor who may promote.
#[test]
fn the_content_list_promotes_and_demotes_a_selection() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app, PLAIN).await;
        let id = create(app, PLAIN, &service_admin()).await;
        assert_eq!(flags(app, id).await, (0, 0));

        let (_, cookies) = common::user_holding(
            app,
            "b158promoter",
            &[
                "access administration pages",
                "edit any content",
                PUBLISH_CONTENT,
            ],
        )
        .await;
        let ip = test_ip_for("b158promoter");
        let (_, page) = get(app, &cookies, "/admin/content", &ip).await;
        assert!(
            page.contains(r#"<option value="promote">"#),
            "no promote action"
        );

        for (action, want) in [
            ("promote", (1, 0)),
            ("sticky", (1, 1)),
            ("demote", (0, 1)),
            ("unsticky", (0, 0)),
        ] {
            let status = bulk(app, &cookies, &ip, action, id).await;
            assert_eq!(status, StatusCode::SEE_OTHER, "{action}");
            assert_eq!(flags(app, id).await, want, "after {action}");
        }
    });
}

/// An editor who may not promote is refused the action and nothing changes.
#[test]
fn bulk_promote_needs_publish_content() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app, PLAIN).await;
        let id = create(app, PLAIN, &service_admin()).await;
        let (_, cookies) = common::user_holding(
            app,
            "b158nopromote",
            &["access administration pages", "edit any content"],
        )
        .await;
        let ip = test_ip_for("b158nopromote");
        let status = bulk(app, &cookies, &ip, "promote", id).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(flags(app, id).await, (0, 0));
    });
}

/// A type's promote and sticky defaults, set on its form, decide a new item's
/// flags when the write says nothing, whoever writes it, and the add form
/// starts from them.
#[test]
fn a_new_item_takes_its_types_promote_and_sticky_defaults() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app, PROMOTED).await;
        let name = common::username("b158promadm");
        let cookies = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple",
                &format!("{name}@test.local"),
            )
            .await;
        let ip = test_ip_for(&name);

        let path = format!("/admin/structure/types/{PROMOTED}/edit");
        let (status, html) = get(app, &cookies, &path, &ip).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains(r#"name="promote_default""#),
            "no promote default on the type form"
        );
        let build = {
            let at = html.find(r#"name="_form_build_id" value=""#).unwrap() + 29;
            html[at..at + html[at..].find('"').unwrap()].to_string()
        };
        let body = format!(
            "_token={}&_form_build_id={}&label={PROMOTED}&machine_name={PROMOTED}\
             &title_label=Title&promote_default=1&sticky_default=1",
            urlencoding::encode(&token_in(&html)),
            urlencoding::encode(&build),
        );
        let status = post_form(app, &cookies, &path, body, &ip).await;
        assert!(status.is_redirection(), "type form refused: {status}");
        let (_, html) = get(app, &cookies, &path, &ip).await;
        let at = html.find(r#"id="promote_default""#).unwrap();
        assert!(
            html[at..at + 120].contains("checked"),
            "the default is not shown"
        );

        // An author without `publish content` still gets the type's default.
        let (author_id, _) = common::user_holding(app, "b158promauthor", &["access content"]).await;
        let author =
            UserContext::authenticated(author_id, vec![format!("create {PROMOTED} content")]);
        let id = create(app, PROMOTED, &author).await;
        assert_eq!(
            flags(app, id).await,
            (1, 1),
            "the type default was not applied"
        );

        // The add form offers the box already ticked.
        let (_, html) = get(app, &cookies, &format!("/item/add/{PROMOTED}"), &ip).await;
        let at = html.find(r#"name="promote""#).expect("a promote box");
        let tag_start = html[..at].rfind('<').unwrap();
        let tag_end = at + html[at..].find('>').unwrap();
        assert!(
            html[tag_start..tag_end].contains("checked"),
            "the add form starts unticked"
        );

        // Put the type back as the other tests expect it.
        app.state
            .content_types()
            .replace(
                PROMOTED,
                PROMOTED,
                None,
                serde_json::json!({ "fields": [] }),
            )
            .await
            .ok();
    });
}
