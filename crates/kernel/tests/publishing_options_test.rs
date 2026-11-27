#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Promote, sticky, author and authored on (B107 Part 2).
//!
//! Both item forms passed `promote: None, sticky: None` and the author was
//! always the creator, dated when they saved: an editor could not put an item
//! on the front page, pin it, credit someone else or backdate it. The form now
//! has a "Publishing options" section, the service decides every value (a
//! holder of `publish content` or `administer content` for the flags,
//! `administer content` alone for author and date), and a save records a
//! revision as any other.
//!
//! These drive the real router: render the form, post back what a browser
//! would, and read the row.

mod common;

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::content::{ADMINISTER_CONTENT, ItemService, PUBLISH_CONTENT, WriteDenied};
use trovato_kernel::gather::{
    DisplayFormat, FilterOperator, FilterValue, PagerConfig, PagerStyle, QueryContext,
    QueryDefinition, QueryDisplay, QueryFilter, QuerySort, SortDirection,
};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, UpdateItem};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const TYPE: &str = "b107_publishing";

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

async fn ensure_type(app: &TestApp) {
    let settings = serde_json::json!({ "fields": [] });
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'B107 Publishing', '', true, 'Title', 'b107_test', $2) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(TYPE)
    .bind(&settings)
    .execute(&app.db)
    .await
    .unwrap();
    app.state
        .content_types()
        .replace(TYPE, "B107 Publishing", None, settings)
        .await
        .ok();
}

fn input_value(html: &str, name: &str) -> Option<String> {
    let needle = format!(r#"name="{name}""#);
    let at = html.find(&needle)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(tag[vat..vend].to_string())
}

/// The CSRF token and build id of the item form on `html`.
fn form_pairs(html: &str) -> String {
    // Scoped to the form that carries the build id: the site layout has a
    // logout form, with a token of its own, before it.
    let at = html.find(r#"name="_form_build_id""#).expect("a build id");
    let scoped = &html[html[..at].rfind("<form").unwrap()..];
    format!(
        "_token={}&_form_build_id={}",
        urlencoding::encode(&input_value(scoped, "_token").unwrap()),
        urlencoding::encode(&input_value(scoped, "_form_build_id").unwrap()),
    )
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    (status, body_string(response).await)
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

fn service_admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec![])
}

async fn seed_item(app: &TestApp, author: Uuid, title: &str) -> Uuid {
    let item = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: TYPE.to_string(),
                title: title.to_string(),
                author_id: author,
                status: Some(1),
                promote: None,
                sticky: None,
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: None,
            },
            &service_admin(),
        )
        .await
        .expect("seed item");
    let id = item.id;
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

#[derive(Debug, sqlx::FromRow)]
struct Row {
    author_id: Uuid,
    created: i64,
    promote: i16,
    sticky: i16,
}

async fn row(app: &TestApp, id: Uuid) -> Row {
    sqlx::query_as("SELECT author_id, created, promote, sticky FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

async fn revisions(app: &TestApp, id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM item_revision WHERE item_id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

async fn admin_login(app: &TestApp, prefix: &str) -> (Uuid, String) {
    let name = common::username(prefix);
    let cookies = app
        .create_and_login_admin(
            &name,
            "correct-horse-battery-staple",
            &format!("{name}@test.local"),
        )
        .await;
    (common::user_id_of(app, &name).await, cookies)
}

/// **The finding.** An administrator sets all four from the admin edit form,
/// the row stores them, and the save is a revision.
#[test]
fn an_administrator_sets_promote_sticky_author_and_authored_on() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let (admin_id, cookies) = admin_login(app, "b107pubadm").await;
        let (other_id, _) = common::user_holding(app, "b107pubauthor", &[]).await;
        let other_name: String = sqlx::query_scalar("SELECT name FROM users WHERE id = $1")
            .bind(other_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        let id = seed_item(app, admin_id, "Publishing probe").await;
        let before = revisions(app, id).await;

        let path = format!("/admin/content/{id}/edit");
        let (status, html) = get(app, &cookies, &path).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Publishing options"), "no section");
        for name in ["promote", "sticky", "author", "authored_on"] {
            assert!(html.contains(&format!(r#"name="{name}""#)), "no {name}");
        }

        let body = format!(
            "{}&title=Publishing+probe&status=1&_status_rendered=1&promote=1&sticky=1\
             &_promotion_rendered=1&author={}&authored_on=2024-03-05T06%3A07&_authoring_rendered=1",
            form_pairs(&html),
            urlencoding::encode(&other_name),
        );
        let (status, page) = post_form(app, &cookies, &path, body).await;
        assert!(status.is_redirection(), "save refused: {status} {page}");

        let stored = row(app, id).await;
        assert_eq!(stored.promote, 1);
        assert_eq!(stored.sticky, 1);
        assert_eq!(stored.author_id, other_id);
        assert_eq!(stored.created, 1_709_618_820, "2024-03-05T06:07 UTC");
        assert_eq!(revisions(app, id).await, before + 1, "a revision recorded");

        // The form shows what was stored.
        let (_, html) = get(app, &cookies, &path).await;
        assert_eq!(
            input_value(&html, "author").as_deref(),
            Some(other_name.as_str())
        );
        assert_eq!(
            input_value(&html, "authored_on").as_deref(),
            Some("2024-03-05T06:07")
        );
    });
}

/// An editor without `administer content` is offered promote and sticky (they
/// hold `publish content`) but not the author, and a forged author is refused
/// with the item unchanged.
#[test]
fn a_non_admin_editor_may_not_set_the_author() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let (editor_id, cookies) = common::user_holding(
            app,
            "b107pubeditor",
            &[
                "access administration pages",
                "edit any content",
                PUBLISH_CONTENT,
            ],
        )
        .await;
        let (other_id, _) = common::user_holding(app, "b107pubother", &[]).await;
        let other_name: String = sqlx::query_scalar("SELECT name FROM users WHERE id = $1")
            .bind(other_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        let id = seed_item(app, editor_id, "Editor probe").await;
        let original = row(app, id).await;

        for path in [
            format!("/admin/content/{id}/edit"),
            format!("/item/{id}/edit"),
        ] {
            let (status, html) = get(app, &cookies, &path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert!(html.contains(r#"name="promote""#), "{path}: no promote");
            assert!(!html.contains(r#"name="author""#), "{path}: author offered");

            let body = format!(
                "{}&title=Editor+probe&author={}&_authoring_rendered=1&authored_on=2001-01-01T00%3A00",
                form_pairs(&html),
                urlencoding::encode(&other_name),
            );
            let (status, page) = post_form(app, &cookies, &path, body).await;
            // The admin form comes back with the refusal on it; `/item` answers
            // 403, as it does for every write gate.
            assert!(
                status == StatusCode::FORBIDDEN
                    || (status == StatusCode::OK && page.contains("requires `administer content`")),
                "{path}: a forged author was not refused: {status}"
            );
            let after = row(app, id).await;
            assert_eq!(
                after.author_id, original.author_id,
                "{path}: author changed"
            );
            assert_eq!(after.created, original.created, "{path}: date changed");
        }

        // Promote alone is theirs to set.
        let path = format!("/admin/content/{id}/edit");
        let (_, html) = get(app, &cookies, &path).await;
        let body = format!(
            "{}&title=Editor+probe&promote=1&_promotion_rendered=1",
            form_pairs(&html)
        );
        let (status, page) = post_form(app, &cookies, &path, body).await;
        assert!(status.is_redirection(), "promote refused: {status} {page}");
        assert_eq!(row(app, id).await.promote, 1);
    });
}

/// An editor holding neither `publish content` nor `administer content` is
/// offered no publishing options, and posting them anyway is refused.
#[test]
fn an_editor_without_publish_content_may_not_promote() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let (editor_id, cookies) = common::user_holding(
            app,
            "b107pubplain",
            &["access administration pages", "edit any content"],
        )
        .await;
        let id = seed_item(app, editor_id, "Plain probe").await;
        let path = format!("/admin/content/{id}/edit");
        let (_, html) = get(app, &cookies, &path).await;
        assert!(!html.contains("Publishing options"));
        let body = format!(
            "{}&title=Plain+probe&sticky=1&_promotion_rendered=1",
            form_pairs(&html)
        );
        let (status, _) = post_form(app, &cookies, &path, body).await;
        assert!(!status.is_redirection(), "sticky accepted: {status}");
        assert_eq!(row(app, id).await.sticky, 0);
    });
}

/// A named author who is no user is a form error, not a save.
#[test]
fn an_unknown_author_is_a_form_error() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let (admin_id, cookies) = admin_login(app, "b107pubunk").await;
        let id = seed_item(app, admin_id, "Unknown author probe").await;
        let path = format!("/admin/content/{id}/edit");
        let (_, html) = get(app, &cookies, &path).await;
        let body = format!(
            "{}&title=Unknown+author+probe&author=nobody-{}&_authoring_rendered=1",
            form_pairs(&html),
            Uuid::now_v7().simple()
        );
        let (status, page) = post_form(app, &cookies, &path, body).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("There is no user named"), "no error shown");
        assert_eq!(row(app, id).await.author_id, admin_id);
    });
}

/// The admin add form creates an item with all four set.
#[test]
fn the_add_form_sets_them_on_create() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let (_admin_id, cookies) = admin_login(app, "b107pubadd").await;
        let (other_id, _) = common::user_holding(app, "b107pubaddauthor", &[]).await;
        let other_name: String = sqlx::query_scalar("SELECT name FROM users WHERE id = $1")
            .bind(other_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        let title = format!("Added {}", Uuid::now_v7().simple());
        let path = format!("/admin/content/add/{TYPE}");
        let (_, html) = get(app, &cookies, &path).await;
        let body = format!(
            "{}&title={}&status=1&_status_rendered=1&promote=1&_promotion_rendered=1\
             &author={}&authored_on=2020-02-02T02%3A02&_authoring_rendered=1",
            form_pairs(&html),
            urlencoding::encode(&title),
            urlencoding::encode(&other_name),
        );
        let (status, page) = post_form(app, &cookies, &path, body).await;
        assert!(status.is_redirection(), "create refused: {status} {page}");
        let (id, author, created, promote, sticky): (Uuid, Uuid, i64, i16, i16) = sqlx::query_as(
            "SELECT id, author_id, created, promote, sticky FROM item WHERE title = $1",
        )
        .bind(&title)
        .fetch_one(&app.db)
        .await
        .unwrap();
        let pool = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM item WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await;
        });
        assert_eq!(author, other_id);
        assert_eq!(created, 1_580_608_920, "2020-02-02T02:02 UTC");
        assert_eq!((promote, sticky), (1, 0));
    });
}

/// At the service: the gates, through `ItemService::update` and `create`.
#[test]
fn the_service_refuses_authoring_without_administer_content() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let (editor_id, _) = common::user_holding(app, "b107pubsvc", &[]).await;
        let id = seed_item(app, editor_id, "Service probe").await;
        let editor = UserContext::authenticated(
            editor_id,
            vec!["edit any content".to_string(), PUBLISH_CONTENT.to_string()],
        );
        let update = |author_id, created, promote| UpdateItem {
            title: None,
            status: None,
            promote,
            sticky: None,
            fields: None,
            log: None,
            stage_id: None,
            author_id,
            created,
        };

        let refused = app
            .state
            .items()
            .update(id, update(Some(Uuid::now_v7()), None, None), &editor)
            .await
            .unwrap_err();
        assert_eq!(
            refused.downcast_ref::<WriteDenied>(),
            Some(&WriteDenied::Authoring)
        );
        let refused = app
            .state
            .items()
            .update(id, update(None, Some(1), None), &editor)
            .await
            .unwrap_err();
        assert_eq!(
            refused.downcast_ref::<WriteDenied>(),
            Some(&WriteDenied::Authoring)
        );

        // Promote is theirs, and the same author is no change.
        let before = revisions(app, id).await;
        app.state
            .items()
            .update(id, update(Some(editor_id), None, Some(1)), &editor)
            .await
            .unwrap();
        assert_eq!(row(app, id).await.promote, 1);
        assert_eq!(revisions(app, id).await, before + 1);

        // A create crediting someone else is refused the same way.
        let refused = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: TYPE.to_string(),
                    title: "Not mine".to_string(),
                    author_id: Uuid::nil(),
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: None,
                    stage_id: None,
                    language: None,
                    log: None,
                },
                &editor,
            )
            .await
            .unwrap_err();
        assert_eq!(
            refused.downcast_ref::<WriteDenied>(),
            Some(&WriteDenied::Authoring)
        );

        // With `administer content` it is allowed.
        let manager = UserContext::authenticated(
            editor_id,
            vec![
                "edit any content".to_string(),
                ADMINISTER_CONTENT.to_string(),
            ],
        );
        app.state
            .items()
            .update(id, update(None, Some(1_000_000_000), None), &manager)
            .await
            .unwrap();
        assert_eq!(row(app, id).await.created, 1_000_000_000);
    });
}

/// The pure gates, case by case.
#[test]
fn the_gates_decide_changes_not_values() {
    let plain = UserContext::authenticated(Uuid::now_v7(), vec![]);
    assert_eq!(
        ItemService::gate_promotion(&plain, None, None, None),
        Ok(())
    );
    assert_eq!(
        ItemService::gate_promotion(&plain, Some(0), Some(0), None),
        Ok(())
    );
    assert_eq!(
        ItemService::gate_promotion(&plain, Some(1), None, None),
        Err(WriteDenied::Promotion)
    );
    assert_eq!(
        ItemService::gate_promotion(&plain, Some(1), Some(1), Some((1, 1))),
        Ok(())
    );
    assert_eq!(
        ItemService::gate_promotion(&plain, None, Some(0), Some((0, 1))),
        Err(WriteDenied::Promotion)
    );
    let publisher = UserContext::authenticated(Uuid::now_v7(), vec![PUBLISH_CONTENT.to_string()]);
    assert_eq!(
        ItemService::gate_promotion(&publisher, Some(1), Some(1), None),
        Ok(())
    );
    assert_eq!(
        ItemService::gate_authoring(&publisher, Some(Uuid::now_v7()), None, None),
        Err(WriteDenied::Authoring)
    );
    let who = Uuid::now_v7();
    assert_eq!(
        ItemService::gate_authoring(&publisher, Some(who), Some(5), Some((who, 5))),
        Ok(())
    );
    let manager = UserContext::authenticated(Uuid::now_v7(), vec![ADMINISTER_CONTENT.to_string()]);
    assert_eq!(
        ItemService::gate_authoring(&manager, Some(who), Some(1), Some((Uuid::nil(), 5))),
        Ok(())
    );
    assert_eq!(
        ItemService::gate_promotion(&manager, Some(1), Some(1), Some((0, 0))),
        Ok(())
    );
}

/// A gather can filter on `promote` and sort `sticky` first, so a front page
/// gather lists promoted items with the pinned ones on top.
#[test]
fn a_gather_filters_on_promote_and_sorts_sticky_first() {
    run_test(async {
        let app = shared_app().await;
        ensure_type(app).await;
        let marker = format!("b107g{}", Uuid::now_v7().simple());
        let mut ids = Vec::new();
        for (label, promote, sticky) in [
            ("plain", 1, 0),
            ("pinned", 1, 1),
            ("hidden", 0, 1),
            ("newest", 1, 0),
        ] {
            let id = seed_item(app, Uuid::nil(), &format!("{marker} {label}")).await;
            app.state
                .items()
                .update(
                    id,
                    UpdateItem {
                        title: None,
                        status: None,
                        promote: Some(promote),
                        sticky: Some(sticky),
                        fields: None,
                        log: None,
                        stage_id: None,
                        author_id: None,
                        created: None,
                    },
                    &service_admin(),
                )
                .await
                .unwrap();
            ids.push(id);
        }

        let filter = |field: &str, operator, value| QueryFilter {
            field: field.to_string(),
            operator,
            value,
            exposed: false,
            exposed_label: None,
            widget: Default::default(),
        };
        let definition = QueryDefinition {
            base_table: "item".to_string(),
            item_type: Some(TYPE.to_string()),
            filters: vec![
                filter(
                    "title",
                    FilterOperator::Contains,
                    FilterValue::String(marker.clone()),
                ),
                filter("promote", FilterOperator::Equals, FilterValue::Integer(1)),
            ],
            sorts: vec![
                QuerySort {
                    field: "sticky".to_string(),
                    direction: SortDirection::Desc,
                    nulls: None,
                },
                QuerySort {
                    field: "id".to_string(),
                    direction: SortDirection::Desc,
                    nulls: None,
                },
            ],
            stage_aware: true,
            ..Default::default()
        };
        let display = QueryDisplay {
            format: DisplayFormat::Table,
            items_per_page: Some(50),
            pager: PagerConfig {
                enabled: true,
                style: PagerStyle::Full,
                show_count: true,
                zero_based: false,
            },
            empty_text: None,
            header: None,
            footer: None,
            canonical_url: None,
            routes: Vec::new(),
            feed: None,
        };
        let ctx = QueryContext {
            current_user_id: None,
            viewer: Some(service_admin()),
            url_args: HashMap::new(),
            language: None,
            ..Default::default()
        };
        let result = app
            .state
            .gather()
            .execute_definition(
                &definition,
                &display,
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &ctx,
            )
            .await
            .expect("gather executes");
        let order: Vec<String> = result
            .items
            .iter()
            .filter_map(|v| v.get("title").and_then(|t| t.as_str()))
            .map(|t| t.rsplit(' ').next().unwrap_or(t).to_string())
            .collect();
        assert_eq!(order, vec!["pinned", "newest", "plain"]);
    });
}
