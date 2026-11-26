#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Content moderation (B158, K-moderation).
//!
//! There was no per-item workflow: an item was published or not, an author's
//! new item was published by default if they could publish, an edit of a
//! published item went live at once, and there was no moderated content list.
//! A moderated type's items now move between Draft, Published and Archived by
//! transitions each role holds, a draft saved over a published item waits
//! beside it while visitors keep the published version, and editors find the
//! drafts at `/admin/content/moderated`.
//!
//! The roles are Umami's: an author holds Create New Draft; an editor every
//! transition but Restore to Draft; an administrator all of them. The pure
//! per-role matrix is `content::moderation`'s unit test; these drive the real
//! write path and pages.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, test_ip_for};
use trovato_kernel::content::WriteDenied;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, Item, UpdateItem};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

const TYPE: &str = "b158_moderated";
const PLAIN: &str = "b158_unmoderated";

const AUTHOR: &[&str] = &[
    "access content",
    "create b158_moderated content",
    "edit own content",
    "view own content",
    "use editorial transition create_new_draft",
];
const EDITOR: &[&str] = &[
    "access content",
    "access administration pages",
    "create b158_moderated content",
    "edit any content",
    "view any content",
    "use editorial transition create_new_draft",
    "use editorial transition publish",
    "use editorial transition archive",
    "use editorial transition archived_published",
];

async fn ensure_types(app: &TestApp) {
    for (name, moderated) in [(TYPE, true), (PLAIN, false)] {
        let settings = serde_json::json!({ "fields": [], "moderation": moderated });
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, $1, '', true, 'Title', 'b158_test', $2) ON CONFLICT (type) DO NOTHING",
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
}

fn user(id: Uuid, permissions: &[&str]) -> UserContext {
    UserContext::authenticated(id, permissions.iter().map(|p| (*p).to_string()).collect())
}

/// A real user holding `permissions` (a row the item's author key needs),
/// as the service sees them.
async fn real(app: &TestApp, prefix: &str, permissions: &[&str]) -> UserContext {
    let (id, _) = common::user_holding(app, prefix, permissions).await;
    user(id, permissions)
}

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec![])
}

fn anonymous() -> UserContext {
    let mut visitor = UserContext::anonymous();
    visitor.permissions = vec!["access content".to_string()];
    visitor
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

fn new_item(item_type: &str, title: &str, author: &UserContext) -> CreateItem {
    CreateItem {
        item_type: item_type.to_string(),
        title: title.to_string(),
        author_id: author.id,
        status: None,
        promote: None,
        sticky: None,
        fields: Some(serde_json::json!({})),
        stage_id: Some(LIVE_STAGE_ID),
        language: Some("en".to_string()),
        log: None,
    }
}

fn edit(title: Option<&str>) -> UpdateItem {
    UpdateItem {
        title: title.map(str::to_string),
        status: None,
        promote: None,
        sticky: None,
        fields: None,
        log: None,
        stage_id: None,
        author_id: None,
        created: None,
    }
}

async fn create(
    app: &TestApp,
    title: &str,
    by: &UserContext,
    state: Option<&str>,
) -> anyhow::Result<Item> {
    let item = app
        .state
        .items()
        .create_moderated(new_item(TYPE, title, by), None, state, by)
        .await?;
    forget(app, item.id);
    Ok(item)
}

async fn moderate(
    app: &TestApp,
    id: Uuid,
    by: &UserContext,
    title: Option<&str>,
    state: Option<&str>,
) -> anyhow::Result<Item> {
    app.state.items().invalidate(id);
    Ok(app
        .state
        .items()
        .update_moderated(id, edit(title), state, by)
        .await?
        .expect("the item"))
}

/// The item row: its title and published status, as visitors get them.
async fn row(app: &TestApp, id: Uuid) -> (String, i16) {
    sqlx::query_as("SELECT title, status FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// The latest revision's title and state.
async fn latest(app: &TestApp, id: Uuid) -> (String, Option<String>) {
    sqlx::query_as(
        "SELECT title, moderation_state FROM item_revision WHERE item_id = $1 \
         ORDER BY created DESC, id DESC LIMIT 1",
    )
    .bind(id)
    .fetch_one(&app.db)
    .await
    .unwrap()
}

fn refused(result: anyhow::Result<Item>, from: &str, to: &str) {
    let error = result.expect_err("the transition should be refused");
    assert_eq!(
        error.downcast_ref::<WriteDenied>(),
        Some(&WriteDenied::Transition {
            from: from.to_string(),
            to: to.to_string()
        }),
        "{error}"
    );
}

async fn visible_to_anonymous(app: &TestApp, id: Uuid) -> bool {
    app.state.items().invalidate(id);
    let item = app.state.items().load(id).await.unwrap().unwrap();
    app.state
        .items()
        .check_access(&item, "view", &anonymous())
        .await
        .unwrap()
}

/// **The finding.** An author's new item is a Draft visitors cannot see, the
/// author may not publish it or create it published, and an editor publishes
/// it.
#[test]
fn an_authors_item_is_a_draft_until_an_editor_publishes_it() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let author = real(app, "b158modau", AUTHOR).await;
        let editor = real(app, "b158moded", EDITOR).await;

        // Create: Draft only.
        refused(
            create(app, "Born published", &author, Some("published")).await,
            "draft",
            "published",
        );
        let item = create(app, "Moderation draft", &author, None)
            .await
            .unwrap();
        assert_eq!(item.status, 0, "a new item is unpublished");
        assert_eq!(latest(app, item.id).await.1.as_deref(), Some("draft"));
        assert!(
            !visible_to_anonymous(app, item.id).await,
            "visitors see a draft"
        );

        // Draft -> Draft: the author's edit stays a draft.
        moderate(app, item.id, &author, Some("Moderation draft 2"), None)
            .await
            .unwrap();
        assert_eq!(
            row(app, item.id).await,
            ("Moderation draft 2".to_string(), 0)
        );
        // Draft -> Published and Draft -> Archived: refused to the author.
        refused(
            moderate(app, item.id, &author, None, Some("published")).await,
            "draft",
            "published",
        );
        refused(
            moderate(app, item.id, &author, None, Some("archived")).await,
            "draft",
            "archived",
        );
        // A status change asks for the same transition.
        let mut publish = edit(None);
        publish.status = Some(1);
        let result = app
            .state
            .items()
            .update_moderated(item.id, publish, None, &author)
            .await;
        refused(result.map(|i| i.unwrap()), "draft", "published");

        // Draft -> Published: the editor's.
        moderate(app, item.id, &editor, None, Some("published"))
            .await
            .unwrap();
        assert_eq!(row(app, item.id).await.1, 1);
        assert_eq!(latest(app, item.id).await.1.as_deref(), Some("published"));
        assert!(visible_to_anonymous(app, item.id).await);

        // A type that is not moderated is untouched: published by a publisher,
        // no state recorded.
        let plain = app
            .state
            .items()
            .create(
                CreateItem {
                    status: Some(1),
                    ..new_item(PLAIN, "Unmoderated", &admin())
                },
                &admin(),
            )
            .await
            .unwrap();
        forget(app, plain.id);
        assert_eq!(plain.status, 1);
        assert_eq!(latest(app, plain.id).await.1, None);
    });
}

/// A draft saved over a published item is a revision only: visitors keep the
/// published title, the editor works on the draft, and publishing it with no
/// new values puts the draft live.
#[test]
fn a_draft_over_a_published_item_leaves_the_published_version_live() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let author = real(app, "b158modau", AUTHOR).await;
        let editor = real(app, "b158moded", EDITOR).await;
        let item = create(app, "Live title", &author, None).await.unwrap();
        moderate(app, item.id, &editor, None, Some("published"))
            .await
            .unwrap();
        let revisions_before: i64 =
            sqlx::query_scalar("SELECT count(*) FROM item_revision WHERE item_id = $1")
                .bind(item.id)
                .fetch_one(&app.db)
                .await
                .unwrap();

        // Published -> Draft by the author, who names no state: their edit of
        // a published item becomes a draft.
        let returned = moderate(app, item.id, &author, Some("Draft title"), None)
            .await
            .unwrap();
        assert_eq!(
            returned.title, "Live title",
            "the service returns what visitors see"
        );
        assert_eq!(row(app, item.id).await, ("Live title".to_string(), 1));
        assert_eq!(
            latest(app, item.id).await,
            ("Draft title".to_string(), Some("draft".to_string()))
        );
        let revisions_after: i64 =
            sqlx::query_scalar("SELECT count(*) FROM item_revision WHERE item_id = $1")
                .bind(item.id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(revisions_after, revisions_before + 1);

        // The editor reads the draft as the latest version; the form works on it.
        app.state.items().invalidate(item.id);
        let stored = app.state.items().load(item.id).await.unwrap().unwrap();
        let latest_version = app
            .state
            .items()
            .latest_version(&stored, &editor)
            .await
            .unwrap();
        assert_eq!(
            latest_version.map(|r| r.title).as_deref(),
            Some("Draft title")
        );
        let (state, working) = app
            .state
            .items()
            .load_for_moderation(&stored)
            .await
            .unwrap()
            .expect("a moderated type");
        assert_eq!(state, "draft");
        assert_eq!(working.title, "Draft title");
        // Visitors are not offered the latest version.
        assert!(
            app.state
                .items()
                .latest_version(&stored, &anonymous())
                .await
                .unwrap()
                .is_none()
        );

        // Draft -> Published by the editor, naming nothing else: the draft goes live.
        moderate(app, item.id, &editor, None, Some("published"))
            .await
            .unwrap();
        assert_eq!(row(app, item.id).await, ("Draft title".to_string(), 1));

        // The history lists the later of two revisions saved in the same
        // second first (`created` holds seconds), as the latest version does:
        // the tie goes to the later id.
        let second = item.created + 60;
        let (earlier, later) = (Uuid::now_v7(), Uuid::now_v7());
        for (id, state) in [(earlier, "published"), (later, "draft")] {
            sqlx::query(
                "INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, log, moderation_state) \
                 VALUES ($1, $2, $3, 'Same second', 0, '{}', $4, 'tie', $5)",
            )
            .bind(id)
            .bind(item.id)
            .bind(author.id)
            .bind(second)
            .bind(state)
            .execute(&app.db)
            .await
            .unwrap();
        }
        let history = Item::get_revisions(&app.db, item.id).await.unwrap();
        assert_eq!(
            history.iter().map(|r| r.id).take(2).collect::<Vec<_>>(),
            vec![later, earlier],
            "same-second revisions are not listed newest first"
        );
    });
}

/// Published -> Archived -> Published by the editor; Archived -> Draft is
/// refused to the editor and the author, and is the administrator's.
#[test]
fn archive_restore_and_restore_to_draft_follow_the_roles() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let author = real(app, "b158modau", AUTHOR).await;
        let editor = real(app, "b158moded", EDITOR).await;
        let item = create(app, "Archive probe", &author, None).await.unwrap();
        moderate(app, item.id, &editor, None, Some("published"))
            .await
            .unwrap();

        // Published -> Archived: refused to the author, the editor's.
        refused(
            moderate(app, item.id, &author, None, Some("archived")).await,
            "published",
            "archived",
        );
        // Published -> Published: refused to the author.
        refused(
            moderate(app, item.id, &author, None, Some("published")).await,
            "published",
            "published",
        );
        moderate(app, item.id, &editor, None, Some("archived"))
            .await
            .unwrap();
        assert_eq!(row(app, item.id).await.1, 0, "archived is unpublished");
        assert!(!visible_to_anonymous(app, item.id).await);

        // Archived -> Draft: refused to the author and the editor.
        refused(
            moderate(app, item.id, &author, None, Some("draft")).await,
            "archived",
            "draft",
        );
        refused(
            moderate(app, item.id, &editor, None, Some("draft")).await,
            "archived",
            "draft",
        );
        // Archived -> Archived: no transition for anyone.
        refused(
            moderate(app, item.id, &admin(), None, Some("archived")).await,
            "archived",
            "archived",
        );
        // An author with no transition from Archived may not save it at all.
        refused(
            moderate(app, item.id, &author, Some("Sneaky"), None).await,
            "archived",
            "archived",
        );

        // Archived -> Published (Restore): the editor's.
        moderate(app, item.id, &editor, None, Some("published"))
            .await
            .unwrap();
        assert!(visible_to_anonymous(app, item.id).await, "restored");

        // Archived -> Draft (Restore to Draft): the administrator's, and the
        // item is then in the moderated list, unpublished.
        moderate(app, item.id, &editor, None, Some("archived"))
            .await
            .unwrap();
        moderate(app, item.id, &admin(), None, Some("draft"))
            .await
            .unwrap();
        assert_eq!(row(app, item.id).await.1, 0);
        assert_eq!(latest(app, item.id).await.1.as_deref(), Some("draft"));
        let listed = Item::list_moderated(&app.db, Some("draft"), 500)
            .await
            .unwrap();
        assert!(
            listed.iter().any(|r| r.id == item.id),
            "a restored draft is in the list"
        );
    });
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
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

async fn post_form(
    app: &TestApp,
    cookies: &str,
    uri: &str,
    body: String,
    ip: &str,
) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("x-forwarded-for", ip)
                .body(Body::from(body))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
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
    let at = html.find(r#"name="_form_build_id""#).expect("a build id");
    let scoped = &html[html[..at].rfind("<form").unwrap()..];
    format!(
        "_token={}&_form_build_id={}",
        urlencoding::encode(&input_value(scoped, "_token").unwrap()),
        urlencoding::encode(&input_value(scoped, "_form_build_id").unwrap()),
    )
}

/// The `option` values of the `moderation_state` select on `html`.
fn moderation_options(html: &str) -> Vec<String> {
    let Some(at) = html.find(r#"name="moderation_state""#) else {
        return Vec::new();
    };
    let end = at + html[at..].find("</select>").unwrap();
    html[at..end]
        .split("<option")
        .skip(1)
        .filter_map(|o| {
            let v = o.find(r#"value=""#)? + 7;
            Some(o[v..v + o[v..].find('"')?].to_string())
        })
        .collect()
}

/// The edit form's moderation control offers exactly the states the user may
/// reach, the save goes through the route to the service, and the moderated
/// list and the latest version page show the draft to editors only.
#[test]
fn the_moderation_control_list_and_latest_version_pages() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let (author_id, author_cookies) = common::user_holding(app, "b158modauthor", AUTHOR).await;
        let (_, editor_cookies) = common::user_holding(app, "b158modeditor", EDITOR).await;
        let ip = test_ip_for("b158mod");
        let author = user(author_id, AUTHOR);
        let editor = real(app, "b158moded", EDITOR).await;
        let item = create(app, "Page probe live", &author, None).await.unwrap();
        moderate(app, item.id, &editor, None, Some("published"))
            .await
            .unwrap();
        let edit_path = format!("/item/{}/edit", item.id);

        // The author is offered Draft only, and no published checkbox.
        let (status, html) = get(app, &author_cookies, &edit_path, &ip).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(moderation_options(&html), vec!["draft"]);
        assert!(
            !html.contains(r#"name="status""#),
            "a published checkbox on a moderated type"
        );
        assert!(html.contains("Current state: <strong>Published</strong>"));

        // The author's save through the form is a draft ahead of the live item.
        let body = format!(
            "{}&title=Page+probe+draft&moderation_state=draft",
            form_pairs(&html)
        );
        let (status, page) = post_form(app, &author_cookies, &edit_path, body, &ip).await;
        assert!(status.is_success(), "author save refused: {status} {page}");
        assert_eq!(row(app, item.id).await, ("Page probe live".to_string(), 1));

        // The editor's form edits the draft and offers Draft and Published:
        // Archive leaves from Published, and the latest revision is a draft.
        let (_, html) = get(app, &editor_cookies, &edit_path, &ip).await;
        assert_eq!(moderation_options(&html), vec!["draft", "published"]);
        assert_eq!(
            input_value(&html, "title").as_deref(),
            Some("Page probe draft")
        );

        // The latest version: the editor reads the draft, a visitor never.
        let latest_path = format!("/item/{}/latest", item.id);
        let (status, html) = get(app, &editor_cookies, &latest_path, &ip).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Page probe draft"));
        let (status, _) = get(app, "", &latest_path, &ip).await;
        assert_ne!(status, StatusCode::OK, "a visitor read the draft");
        let (status, html) = get(app, "", &format!("/item/{}", item.id), &ip).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Page probe live") && !html.contains("Page probe draft"));

        // The moderated list: the editor's, not the author's or a visitor's.
        let (status, html) = get(app, &editor_cookies, "/admin/content/moderated", &ip).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Page probe draft"), "the draft is not listed");
        let (status, _) = get(app, &author_cookies, "/admin/content/moderated", &ip).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = get(app, "", "/admin/content/moderated", &ip).await;
        assert_ne!(status, StatusCode::OK);

        // The editor publishes from the form.
        let (_, html) = get(app, &editor_cookies, &edit_path, &ip).await;
        let body = format!(
            "{}&title=Page+probe+draft&moderation_state=published",
            form_pairs(&html)
        );
        let (status, page) = post_form(app, &editor_cookies, &edit_path, body, &ip).await;
        assert!(
            status.is_success(),
            "editor publish refused: {status} {page}"
        );
        assert_eq!(row(app, item.id).await, ("Page probe draft".to_string(), 1));

        // An archived item offers the editor Restore only.
        moderate(app, item.id, &editor, None, Some("archived"))
            .await
            .unwrap();
        let (_, html) = get(app, &editor_cookies, &edit_path, &ip).await;
        assert_eq!(moderation_options(&html), vec!["published"]);
    });
}
