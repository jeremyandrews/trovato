#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Item views go through the item access path.
//!
//! The front page's promoted listing read `list_promoted` with no viewer, so a
//! visitor without `access content` (or one a plugin denied an item to) was
//! shown every promoted item anyway, and its teaser cut the filtered HTML at
//! 200 characters, which could end inside a tag and leave it open. The
//! front-end edit form checked `edit` only, so a user who may not see an item
//! learnt from a 403 that it exists, where every sibling route answers 404.
//!
//! One test here takes `access content` away from the anonymous role, so the
//! tests in this binary run one at a time.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::role::well_known::ANONYMOUS_ROLE_ID;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const ITEM_TYPE: &str = "b87b_view_access";

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

async fn ensure_item_type(app: &TestApp) {
    let settings = serde_json::json!({ "fields": [] });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'B87b view access', 'Fixture type', true, 'Title', 'core', $2) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(ITEM_TYPE)
    .bind(&settings)
    .execute(&app.db)
    .await
    .expect("seed item type");
    app.state
        .content_types()
        .replace(
            ITEM_TYPE,
            "B87b view access",
            Some("Fixture type"),
            settings,
        )
        .await
        .ok();
}

/// A published, promoted, sticky item, dated ahead of everything else so it
/// heads the front page listing. Deleted when the test ends.
async fn promoted_item(app: &TestApp, title: &str, fields: serde_json::Value) -> Uuid {
    ensure_item_type(app).await;
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: ITEM_TYPE.to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(1),
                sticky: Some(1),
                fields: Some(fields),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: None,
            },
            &admin(),
        )
        .await
        .expect("create item")
        .id;
    sqlx::query("UPDATE item SET created = $1 WHERE id = $2")
        .bind(chrono::Utc::now().timestamp() + 1_000_000)
        .bind(id)
        .execute(&app.db)
        .await
        .expect("date item");
    let items = std::sync::Arc::clone(app.state.items());
    common::defer_cleanup(move || async move {
        let _ = items.delete(id, &admin()).await;
    });
    id
}

async fn front_page_html(app: &TestApp) -> String {
    trovato_kernel::models::SiteConfig::delete(&app.db, "site_front_page")
        .await
        .expect("clear front page");
    // The front page path is cached; this server reads it again at once.
    app.state.front_page().clear();
    let response = app
        .request(Request::get("/").body(Body::empty()).unwrap())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The promoted listing is the viewer's: an anonymous visitor sees a promoted
/// item while their role holds `access content`, and not once it does not.
#[test]
fn the_promoted_listing_asks_for_access_content() {
    run_test(async {
        let _serial = SERIAL.lock().await;
        let app = shared_app().await;
        let title = format!("b87b promoted {}", Uuid::now_v7().simple());
        promoted_item(app, &title, serde_json::json!({})).await;

        let had = trovato_kernel::models::Role::get_permissions(&app.db, ANONYMOUS_ROLE_ID)
            .await
            .unwrap()
            .iter()
            .any(|p| p == "access content");
        if !had {
            app.state
                .roles()
                .add_permission(ANONYMOUS_ROLE_ID, "access content")
                .await
                .unwrap();
        }
        assert!(
            front_page_html(app).await.contains(&title),
            "with `access content` the anonymous visitor sees the promoted item"
        );

        app.state
            .roles()
            .remove_permission(ANONYMOUS_ROLE_ID, "access content")
            .await
            .unwrap();
        let roles = std::sync::Arc::clone(app.state.roles());
        common::defer_cleanup(move || async move {
            let _ = roles
                .add_permission(ANONYMOUS_ROLE_ID, "access content")
                .await;
        });
        common::settle_cache_generations().await;
        let html = front_page_html(app).await;
        app.state
            .roles()
            .add_permission(ANONYMOUS_ROLE_ID, "access content")
            .await
            .unwrap();
        assert!(
            !html.contains(&title),
            "without `access content` the promoted item must not be listed"
        );
    });
}

/// A teaser is cut from the body's plain text, so a long formatted body never
/// leaves a tag open in the listing.
#[test]
fn a_teaser_is_cut_from_plain_text() {
    run_test(async {
        let _serial = SERIAL.lock().await;
        let app = shared_app().await;
        let title = format!("b87b teaser {}", Uuid::now_v7().simple());
        let body = format!("<p><strong>{}</strong></p>", "word ".repeat(80));
        promoted_item(
            app,
            &title,
            serde_json::json!({ "body": { "value": body, "format": "filtered_html" } }),
        )
        .await;

        let html = front_page_html(app).await;
        let at = html.find(&title).expect("the item is listed");
        let teaser_at = at + html[at..].find("blog-teaser__summary").expect("a teaser");
        let teaser_end = teaser_at + html[teaser_at..].find("</div>").unwrap();
        let teaser = &html[teaser_at..teaser_end];
        assert!(teaser.contains("word word"), "the teaser has the text");
        assert!(
            !teaser.contains("<strong>"),
            "the teaser is plain text, cut before filtering: {teaser}"
        );
        assert!(teaser.contains("..."), "and says it was cut");
    });
}

/// The front-end edit form answers an item the user may not see with 404, as
/// the item page does, and keeps 403 for one they see and may not edit.
#[test]
fn the_edit_form_hides_an_item_the_user_may_not_see() {
    run_test(async {
        let _serial = SERIAL.lock().await;
        let app = shared_app().await;
        ensure_item_type(app).await;
        let (_, cookies) = common::user_holding(app, "b87b-edit", &["access content"]).await;

        let mut ids = Vec::new();
        for status in [0, 1] {
            let id = app
                .state
                .items()
                .create(
                    CreateItem {
                        item_type: ITEM_TYPE.to_string(),
                        title: format!("b87b edit form {}", Uuid::now_v7().simple()),
                        author_id: Uuid::nil(),
                        status: Some(status),
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
            let items = std::sync::Arc::clone(app.state.items());
            common::defer_cleanup(move || async move {
                let _ = items.delete(id, &admin()).await;
            });
            ids.push(id);
        }

        let mut statuses = Vec::new();
        for id in &ids {
            let response = app
                .request_with_cookies(
                    Request::get(format!("/item/{id}/edit"))
                        .header("x-forwarded-for", common::test_ip_for("b87b-edit"))
                        .body(Body::empty())
                        .unwrap(),
                    &cookies,
                )
                .await;
            statuses.push(response.status());
        }
        assert_eq!(
            statuses,
            vec![StatusCode::NOT_FOUND, StatusCode::FORBIDDEN],
            "an unpublished item the user may not see is 404; a published one they may not edit is 403"
        );
    });
}
