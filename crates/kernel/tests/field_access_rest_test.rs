#![allow(clippy::unwrap_used, clippy::expect_used)]
//! FR-8 Story 3.3 — HTTP-boundary tests for the REST + comment read-path
//! adoption of the shared access-aware seam.
//!
//! These assert **item-level** enforcement at each surface's own boundary: an
//! unpublished item (and comments on it) is invisible (404) to an anonymous
//! caller and visible to a privileged one. Field-level dropping through the same
//! seam is validated end-to-end via the reference plugin in Story 3.8
//! (`field_access_plugin_test.rs`); here the structural fix (routing REST/comment
//! reads through the seam that closes A1/AC-R1/AC-R4) is exercised over real HTTP.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

async fn make_item(app: &common::TestApp, title: &str, status: i16) -> Uuid {
    let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
    app.state
        .items()
        .create(
            CreateItem {
                item_type: "conference".to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({ "field_city": { "value": "Barga" } })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("3.3 rest test".to_string()),
            },
            &admin,
        )
        .await
        .expect("create")
        .id
}

#[test]
fn rest_get_item_hides_unpublished_from_anonymous() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let draft = make_item(app, "REST Draft", 0).await;
        let published = make_item(app, "REST Public", 1).await;

        // Anonymous: unpublished item is 404 (item-level access via the seam).
        let resp = app
            .request(
                Request::get(format!("/api/item/{draft}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "anon must not see an unpublished item over REST"
        );

        // Anonymous: published item on the live stage is visible.
        let resp = app
            .request(
                Request::get(format!("/api/item/{published}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "anon must see a published item over REST"
        );
    });
}

#[test]
fn ssr_view_item_hides_unpublished_from_anonymous() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let draft = make_item(app, "SSR Draft", 0).await;

        // SSR HTML view enforces item-level access (view_item -> load_for_view).
        let resp = app
            .request(
                Request::get(format!("/item/{draft}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "anon must not see an unpublished item over SSR"
        );
    });
}

/// A1 / AC-R1 CLOSED-BY proof for the REST **list** endpoints. The audit's A1
/// covers `get_item_api`, `list_items_api` (`/api/items`) and `list_items_by_type`
/// (`/api/items/{type}`); the single-item case is proven above. Story 3.3 routed
/// all three through the shared `filter_page_for_view` seam, so an anonymous list
/// must exclude an unpublished item while including a published one.
#[test]
fn rest_list_endpoints_hide_unpublished_from_anonymous() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let _draft = make_item(app, "REST List Draft Unique", 0).await;
        let published = make_item(app, "REST List Public Unique", 1).await;

        for path in ["/api/items?type=conference", "/api/items/conference"] {
            let resp = app
                .request(Request::get(path).body(Body::empty()).unwrap())
                .await;
            assert_eq!(resp.status(), StatusCode::OK, "list {path} should be 200");
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains("REST List Draft Unique"),
                "anon list {path} must not include an unpublished item"
            );
            assert!(
                text.contains(&published.to_string()) || text.contains("REST List Public Unique"),
                "anon list {path} must include the published item"
            );
        }
    });
}

#[test]
fn rest_comments_hidden_for_inaccessible_parent() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let draft = make_item(app, "REST Draft Comments", 0).await;

        // Comments on an item the anon caller cannot see return 404 (no existence
        // leak), rather than an empty list.
        let resp = app
            .request(
                Request::get(format!("/api/item/{draft}/comments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "anon must not read comments on an inaccessible item"
        );
    });
}

// =============================================================================
// The read paths that loaded an item and never asked the seam about it.
//
// Every test below pairs a negative with a positive: the fix has to hide what
// the viewer may not see *and* still show what they may, or it could pass by
// returning nothing at all.
// =============================================================================

async fn get_as(
    app: &common::TestApp,
    path: &str,
    cookies: &str,
    bucket: &str,
) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(path)
                .header("x-forwarded-for", common::test_ip_for(bucket))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Problem 1 — the revision list required a login and nothing else, so the
/// titles and log messages of every draft were readable by anyone with an
/// account. It is now the same bar the revert button on the same page applies.
#[test]
fn the_revision_list_is_held_to_view_then_edit() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let draft = make_item(app, "Revision Seam Draft", 0).await;

        // A logged-in user who cannot view the draft is told it does not exist.
        let (_, nosy) = common::user_holding(app, "revseam-nosy", &[]).await;
        let (status, html) = get_as(
            app,
            &format!("/item/{draft}/revisions"),
            &nosy,
            "revseam-nosy",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "a user who cannot view the draft must not read its revision history, got {html}"
        );

        // A user who may view it but not edit it is refused, as the revert
        // button on the same page already refuses them.
        let (_, reader) = common::user_holding(app, "revseam-reader", &["view any content"]).await;
        let (status, html) = get_as(
            app,
            &format!("/item/{draft}/revisions"),
            &reader,
            "revseam-reader",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "reading the revision history is an editor's view, not a reader's, got {html}"
        );

        // And an editor still gets the page.
        let (_, editor) = common::user_holding(
            app,
            "revseam-editor",
            &["view any content", "edit any content"],
        )
        .await;
        let (status, html) = get_as(
            app,
            &format!("/item/{draft}/revisions"),
            &editor,
            "revseam-editor",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "an editor must still see the history, got {html}"
        );
        assert!(
            html.contains("Revision Seam Draft"),
            "the editor's page must name the item, got {html}"
        );
    });
}

/// Problem 6 — `create_comment_inner` loaded the item to notify its author and
/// never asked whether the commenter may see it, so `post comments` was enough
/// to comment on (and learn the existence of) a draft.
#[test]
fn a_comment_cannot_be_posted_on_an_item_the_author_cannot_see() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        app.ensure_plugin_enabled("trovato_comments").await;

        let draft = make_item(app, "Comment Seam Draft", 0).await;
        let published = make_item(app, "Comment Seam Public", 1).await;

        let (_, cookies) = common::user_holding(app, "cmtseam", &["post comments"]).await;

        // The draft answers exactly as a missing item does.
        let (status, body) = post_comment_to(app, &cookies, draft).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "commenting on an unviewable item must answer as for a missing one, got {body}"
        );
        assert!(
            body.contains("Item not found"),
            "the refusal must not distinguish denied from missing, got {body}"
        );

        // The same user may still comment on the published item.
        let (status, body) = post_comment_to(app, &cookies, published).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the same commenter must still reach a published item, got {body}"
        );
    });
}

/// POST a comment, returning the status and body.
async fn post_comment_to(
    app: &common::TestApp,
    cookies: &str,
    item_id: Uuid,
) -> (StatusCode, String) {
    // A CSRF token, read off a rendered page in this session.
    let response = app
        .request_with_cookies(
            Request::get("/")
                .header("x-forwarded-for", common::test_ip_for("cmtseam"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let fresh = common::extract_cookies(&response);
    let cookies = if fresh.is_empty() {
        cookies.to_string()
    } else {
        fresh
    };
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let marker = "name=\"csrf-token\" content=\"";
    let start = html.find(marker).expect("csrf meta tag") + marker.len();
    let token = html[start..].split('"').next().expect("token").to_string();

    let response = app
        .request_with_cookies(
            Request::post(format!("/api/item/{item_id}/comments"))
                .header("content-type", "application/json")
                .header("X-CSRF-Token", &token)
                .header("x-forwarded-for", common::test_ip_for("cmtseam"))
                .body(Body::from(
                    serde_json::json!({ "body": "Is this item even here?" }).to_string(),
                ))
                .unwrap(),
            &cookies,
        )
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

// =============================================================================
// Revision history for editors: who changed it, preview, compare, revert.
//
// The history page answered 500 for every item with a revision until c137f2b,
// and nothing posted a revert as a non-admin editor, so a broken revert hid
// behind the broken page. These drive every revision route over HTTP as an
// editor who is not an administrator.
// =============================================================================

/// The username behind an id, read from the database.
async fn name_of(app: &common::TestApp, id: Uuid) -> String {
    sqlx::query_scalar("SELECT name FROM users WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .expect("read username")
}

/// The context an editor holding exactly `permissions` acts with.
fn editor_ctx(id: Uuid, permissions: &[&str]) -> UserContext {
    UserContext::authenticated(id, permissions.iter().map(|p| p.to_string()).collect())
}

/// Edit an item as `user`: a new title, and `field_city` set to `city`.
async fn edit_as(app: &common::TestApp, user: &UserContext, id: Uuid, title: &str, city: &str) {
    app.state
        .items()
        .update(
            id,
            trovato_kernel::models::UpdateItem {
                author_id: None,
                created: None,
                title: Some(title.to_string()),
                status: None,
                promote: None,
                sticky: None,
                fields: Some(serde_json::json!({ "field_city": { "value": city } })),
                log: Some(format!("moved to {city}")),
                stage_id: None,
            },
            user,
        )
        .await
        .expect("edit")
        .expect("item exists");
}

/// The item's revision ids, oldest first.
async fn revision_ids(app: &common::TestApp, id: Uuid) -> Vec<Uuid> {
    sqlx::query_scalar("SELECT id FROM item_revision WHERE item_id = $1 ORDER BY created, id")
        .bind(id)
        .fetch_all(&app.db)
        .await
        .expect("read revision ids")
}

const EDITOR_PERMS: &[&str] = &["view any content", "edit any content"];

#[test]
fn an_editor_reads_who_changed_it_previews_compares_and_reverts() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let id = make_item(app, "Revision History Original", 1).await;
        let (editor_id, cookies) = common::user_holding(app, "revhist-editor", EDITOR_PERMS).await;
        let editor = editor_ctx(editor_id, EDITOR_PERMS);
        let editor_name = name_of(app, editor_id).await;
        edit_as(app, &editor, id, "Revision History Second", "Lucca").await;
        edit_as(app, &editor, id, "Revision History Third", "Pisa").await;

        let revs = revision_ids(app, id).await;
        assert_eq!(revs.len(), 3, "a create and two edits are three revisions");
        let (first, third) = (revs[0], revs[2]);

        // The history: three revisions, each with a preview link, and who made them.
        let history = format!("/item/{id}/revisions");
        let (status, html) = get_as(app, &history, &cookies, "revhist-editor").await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the history must render, got {html}"
        );
        for rev in &revs {
            assert!(
                html.contains(&format!("/item/{id}/revisions/{rev}")),
                "the history must link a preview of revision {rev}, got {html}"
            );
        }
        assert!(
            html.contains(&editor_name),
            "the history must name the editor who made the edits, got {html}"
        );

        // The preview of the first revision: its title and its city, not today's.
        let (status, html) = get_as(
            app,
            &format!("/item/{id}/revisions/{first}"),
            &cookies,
            "revhist-editor",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the preview must render, got {html}"
        );
        assert!(
            html.contains("Revision History Original"),
            "the preview must show the revision's title, got {html}"
        );
        assert!(
            html.contains("Barga"),
            "the preview must show the revision's field value, got {html}"
        );
        assert!(
            !html.contains("Pisa"),
            "the preview must not show the current value, got {html}"
        );

        // The compare of first and third: the changed field's old and new text.
        let (status, html) = get_as(
            app,
            &format!("/item/{id}/revisions/compare?from={first}&to={third}"),
            &cookies,
            "revhist-editor",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the compare must render, got {html}"
        );
        // The title and the city are diffed word by word: what changed is
        // marked as removed and added, and what stayed is plain text.
        for expected in [
            "City",
            r#"<del class="diff-removed">Barga</del>"#,
            r#"<ins class="diff-added">Pisa</ins>"#,
            r#"<del class="diff-removed">Original</del>"#,
            r#"<ins class="diff-added">Third</ins>"#,
        ] {
            assert!(
                html.contains(expected),
                "the compare must show `{expected}`, got {html}"
            );
        }

        // Revert to the first, with the revert form the history page carries:
        // each row's Revert button joins it and names its own revision.
        let (_, html) = get_as(app, &history, &cookies, "revhist-editor").await;
        assert!(
            html.contains(&format!(
                r#"form="revision_revert_form" formaction="/item/{id}/revert/{first}""#
            )),
            "the first revision's Revert joins the revert form"
        );
        let fields = common::form_fields(&html, "revision_revert_form");
        let response = app
            .request_with_cookies(
                Request::post(format!("/item/{id}/revert/{first}"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("x-forwarded-for", common::test_ip_for("revhist-editor"))
                    .body(Body::from(fields))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert!(
            response.status().is_redirection(),
            "a revert must redirect back to the history, got {}",
            response.status()
        );
        let item = app.state.items().load(id).await.unwrap().unwrap();
        assert_eq!(
            item.title, "Revision History Original",
            "the title is restored"
        );
        let revs = revision_ids(app, id).await;
        assert_eq!(
            revs.len(),
            4,
            "a revert is a fourth revision, not a rewrite"
        );
        let log: Option<String> = sqlx::query_scalar("SELECT log FROM item_revision WHERE id = $1")
            .bind(revs[3])
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert!(
            log.as_deref()
                .is_some_and(|l| l.contains("Reverted") && l.contains(&first.to_string())),
            "the fourth revision's log must name the revert, got {log:?}"
        );
    });
}

#[test]
fn preview_and_compare_are_held_to_view_then_edit_and_to_the_item() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let draft = make_item(app, "Revision Gate Draft", 0).await;
        let other = make_item(app, "Revision Gate Other", 0).await;
        let rev = revision_ids(app, draft).await[0];
        let foreign = revision_ids(app, other).await[0];
        let preview = format!("/item/{draft}/revisions/{rev}");
        let compare = format!("/item/{draft}/revisions/compare?from={rev}&to={rev}");

        // Cannot view the item: it does not exist for them.
        let (_, nosy) = common::user_holding(app, "revgate-nosy", &[]).await;
        for path in [&preview, &compare] {
            let (status, html) = get_as(app, path, &nosy, "revgate-nosy").await;
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "{path} for a non-viewer, got {html}"
            );
        }

        // May view it but not edit it: refused.
        let (_, reader) = common::user_holding(app, "revgate-reader", &["view any content"]).await;
        for path in [&preview, &compare] {
            let (status, html) = get_as(app, path, &reader, "revgate-reader").await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{path} for a reader, got {html}"
            );
        }

        // An editor, asking for a revision of another item through this one.
        let (_, editor) = common::user_holding(app, "revgate-editor", EDITOR_PERMS).await;
        for path in [
            format!("/item/{draft}/revisions/{foreign}"),
            format!("/item/{draft}/revisions/compare?from={rev}&to={foreign}"),
            format!("/item/{draft}/revisions/compare?from={foreign}&to={rev}"),
        ] {
            let (status, html) = get_as(app, &path, &editor, "revgate-editor").await;
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "{path}: a revision of another item is not found here, got {html}"
            );
        }
        // And the same editor reaches both pages for this item's own revision.
        for path in [&preview, &compare] {
            let (status, html) = get_as(app, path, &editor, "revgate-editor").await;
            assert_eq!(status, StatusCode::OK, "{path} for the editor, got {html}");
        }

        // Anonymous: sent to log in.
        for path in [&preview, &compare] {
            let response = app
                .request(
                    Request::get(path.as_str())
                        .header("x-forwarded-for", common::test_ip_for("revgate-anon"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await;
            assert_eq!(
                response.status(),
                axum::http::StatusCode::UNAUTHORIZED,
                "{path} for anonymous must be refused as nobody signed in"
            );
        }
    });
}

/// The editor's way in: the edit form they may open carries the Revisions tab.
#[test]
fn a_non_admin_editor_reaches_the_history_from_the_edit_form() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let id = make_item(app, "Revision Reach Item", 1).await;
        let (_, cookies) = common::user_holding(app, "revreach-editor", EDITOR_PERMS).await;
        let (status, html) = get_as(
            app,
            &format!("/admin/content/{id}/edit"),
            &cookies,
            "revreach-editor",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the editor opens the edit form, got {html}"
        );
        // Autoescape writes `/` as `&#x2F;`, which a browser reads as `/`.
        // On `main` the tab macro escaped twice, giving `&amp;#x2F;`, which a
        // browser reads as the literal text `&#x2F;`: a link to nowhere.
        let as_a_browser_reads_it = html.replace("&#x2F;", "/");
        assert!(
            as_a_browser_reads_it.contains(&format!(r#"href="/item/{id}/revisions""#)),
            "the edit form must link the revision history, got {html}"
        );
    });
}

// -----------------------------------------------------------------------------
// Query counts: a fixed number per page, whatever the revisions and authors.
// -----------------------------------------------------------------------------

/// Counts `sqlx::query` events: one per statement sqlx runs.
struct QueryCounter(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for QueryCounter {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() == "sqlx::query" {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// How many statements one GET runs, after a warm-up GET has filled the caches.
async fn queries_for(app: &common::TestApp, path: &str, cookies: &str, bucket: &str) -> usize {
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::layer::SubscriberExt;

    common::scoped_subscribers_see_every_callsite();
    common::settle_cache_generations().await;
    let (status, html) = get_as(app, path, cookies, bucket).await;
    assert_eq!(status, StatusCode::OK, "{path} must render, got {html}");

    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let subscriber = tracing_subscriber::registry().with(QueryCounter(count.clone()));
    let (status, _) = get_as(app, path, cookies, bucket)
        .with_subscriber(subscriber)
        .await;
    assert_eq!(status, StatusCode::OK);
    count.load(std::sync::atomic::Ordering::SeqCst)
}

#[test]
fn revision_pages_make_a_fixed_number_of_queries() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let (viewer_id, cookies) = common::user_holding(app, "revcount-viewer", EDITOR_PERMS).await;
        let _ = viewer_id;

        // A small history by one author, and a larger one by three.
        let small = make_item(app, "Revision Count Small", 1).await;
        let (a, _) = common::user_holding(app, "revcount-a", EDITOR_PERMS).await;
        edit_as(
            app,
            &editor_ctx(a, EDITOR_PERMS),
            small,
            "Revision Count Small",
            "One",
        )
        .await;

        let large = make_item(app, "Revision Count Large", 1).await;
        let (b, _) = common::user_holding(app, "revcount-b", EDITOR_PERMS).await;
        let (c, _) = common::user_holding(app, "revcount-c", EDITOR_PERMS).await;
        for (n, author) in [a, b, c, a, b, c].into_iter().enumerate() {
            edit_as(
                app,
                &editor_ctx(author, EDITOR_PERMS),
                large,
                "Revision Count Large",
                &format!("City {n}"),
            )
            .await;
        }

        let mut counts = Vec::new();
        for id in [small, large] {
            let revs = revision_ids(app, id).await;
            let (first, last) = (revs[0], revs[revs.len() - 1]);
            let pages = [
                format!("/item/{id}/revisions"),
                format!("/item/{id}/revisions/{first}"),
                format!("/item/{id}/revisions/compare?from={first}&to={last}"),
            ];
            let mut per_item = Vec::new();
            for page in &pages {
                per_item.push(queries_for(app, page, &cookies, "revcount-viewer").await);
            }
            counts.push((revs.len(), per_item));
        }
        eprintln!(
            "revision page query counts (revisions, [history, preview, compare]): {counts:?}"
        );

        let (small_counts, large_counts) = (&counts[0].1, &counts[1].1);
        assert!(
            small_counts.iter().all(|&n| n > 0),
            "the counter must see the statements at all, got {counts:?}"
        );
        assert_eq!(
            small_counts, large_counts,
            "history, preview and compare must make the same number of queries for 2 \
             revisions by 2 authors as for 7 by 4"
        );
    });
}
