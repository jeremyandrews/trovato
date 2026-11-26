#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B248 — the search total, and the rows a page actually holds, must agree.
//!
//! Before this, `SearchService::search_with` counted rows with one predicate
//! (`status = 1 OR author_id = $viewer`, filtered to `stage_ids`) and routed the
//! page through `ItemService::check_access` after the fact
//! (`ItemService::filter_search_results`). A row the count included and the
//! access check denied made `total` bigger than what the pager ever delivered,
//! which told a viewer that a hidden item matched their query.
//!
//! `ItemService::search_visibility` is now the one place that decides what a
//! viewer's search may return, and `SearchService::search` builds both the
//! count and the page from it, so they can no longer drift apart. These tests
//! reproduce each condition under which the old coarse predicate and
//! `check_access` disagreed (see `search_query_test.rs` for the two permission
//! gate cases, which need a different, DB-role-mutating harness) and assert,
//! for each: `total` equals the number of rows summed across every page, no
//! page is short of the limit except the last, and the last page is the last
//! (`offset + limit >= total`).
//!
//! Two of these — the author's own unpublished item without the permission
//! that shows it, and the internal-stage item — reproduce the bug exactly:
//! checked out against `main` with `SearchService::search`'s old two-argument
//! shape (`stage_ids: &[Uuid], user_id: Option<Uuid>`) and no
//! `ItemService::search_visibility`, both read `total = 2, shown = 1`. On this
//! branch both read `total = shown`.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use common::{TestApp, run_test, shared_app};
use trovato_kernel::content::VIEW_OWN_UNPUBLISHED;
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::search::{SearchResults, SearchService};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

fn anon_with_access() -> UserContext {
    let mut ctx = UserContext::anonymous();
    ctx.permissions = vec!["access content".to_string()];
    ctx
}

async fn make_item(
    app: &TestApp,
    title: &str,
    author_id: Uuid,
    status: i16,
    stage_id: Uuid,
) -> Uuid {
    app.state
        .items()
        .create(
            CreateItem {
                item_type: "conference".to_string(),
                title: title.to_string(),
                author_id,
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({
                    "field_body": { "value": format!("{title} body detail") }
                })),
                stage_id: Some(stage_id),
                language: Some("en".to_string()),
                log: Some("B248 total consistency test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create")
        .id
}

/// One page, read through [`ItemService::filter_search_results`] as `viewer` —
/// the real seam, so this measures what a page actually shows, not only what
/// the coarse SQL returned.
async fn shown_page(
    app: &TestApp,
    search: &SearchService,
    query: &str,
    visibility: &trovato_kernel::search::SearchVisibility,
    viewer: &UserContext,
    limit: i64,
    offset: i64,
) -> (SearchResults, usize) {
    let raw = search
        .search_unrecorded(query, visibility, limit, offset)
        .await
        .expect("search");
    let shown = app
        .state
        .items()
        .filter_search_results(raw.results.clone(), viewer)
        .await
        .len();
    (raw, shown)
}

/// Walk every page of `query` for `viewer` and assert: `total` is stable page
/// to page, every page but the last is full, the last page holds exactly the
/// remainder, and the rows actually shown (post-seam) sum to `total` — so a
/// count can never promise a row the pager does not deliver.
async fn assert_total_matches_every_page(
    app: &TestApp,
    query: &str,
    viewer: &UserContext,
    stage_ids: &[Uuid],
    limit: i64,
) {
    let visibility = app.state.items().search_visibility(viewer, stage_ids).await;
    let search = app.state.search();

    let (first, _) = shown_page(app, search, query, &visibility, viewer, limit, 0).await;
    let total = first.total;

    let mut shown_total = 0i64;
    let mut offset = 0i64;
    loop {
        let (page, shown) =
            shown_page(app, search, query, &visibility, viewer, limit, offset).await;
        assert_eq!(
            page.total, total,
            "total must not change between pages at offset {offset}"
        );
        let remaining = total - offset;
        let expected = remaining.clamp(0, limit);
        assert_eq!(
            shown as i64,
            expected,
            "offset {offset}: expected {expected} shown of total {total}, got {shown} \
             (raw SQL returned {} rows)",
            page.results.len()
        );
        shown_total += shown as i64;
        offset += limit;
        if offset >= total.max(1) {
            break;
        }
    }
    assert_eq!(
        shown_total, total,
        "rows actually shown across every page must sum to the reported total, for {query:?}"
    );
}

/// Control case: an anonymous visitor holding `access content`, several
/// published items, several pages. No gap here before or after B248 — this
/// pins the pagination arithmetic the other cases also rely on.
#[test]
fn anonymous_total_matches_every_page() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        let term = format!("zqxb248anon{}", Uuid::now_v7().simple());
        for i in 0..5 {
            make_item(
                app,
                &format!("{term} item {i}"),
                Uuid::nil(),
                1,
                LIVE_STAGE_ID,
            )
            .await;
        }

        assert_total_matches_every_page(app, &term, &anon_with_access(), &[LIVE_STAGE_ID], 2).await;
    });
}

/// The regression this bug fix is for: an author's own unpublished item is
/// pulled into the coarse SQL's `author_id = $viewer` arm whether or not they
/// hold the permission that lets `check_access` show it back to them. Without
/// `view own unpublished content` the draft must not be counted at all; with
/// it, both the published item and the draft are counted and shown.
#[test]
fn own_unpublished_item_is_counted_only_with_the_permission_that_shows_it() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let (author, _) = common::user_holding(app, "b248ownunpub", &[]).await;

        let term = format!("zqxb248own{}", Uuid::now_v7().simple());
        make_item(
            app,
            &format!("{term} published"),
            Uuid::nil(),
            1,
            LIVE_STAGE_ID,
        )
        .await;
        make_item(app, &format!("{term} draft"), author, 0, LIVE_STAGE_ID).await;

        // Without the permission: only the published item is ever counted.
        let without = UserContext::authenticated(author, vec!["access content".to_string()]);
        assert_total_matches_every_page(app, &term, &without, &[LIVE_STAGE_ID], 10).await;
        let visibility = app
            .state
            .items()
            .search_visibility(&without, &[LIVE_STAGE_ID])
            .await;
        let page = app
            .state
            .search()
            .search_unrecorded(&term, &visibility, 10, 0)
            .await
            .expect("search");
        assert_eq!(
            page.total, 1,
            "the draft must not be a candidate at all without the permission"
        );

        // With the permission: the draft is both counted and shown.
        let with = UserContext::authenticated(
            author,
            vec![
                "access content".to_string(),
                VIEW_OWN_UNPUBLISHED.to_string(),
            ],
        );
        assert_total_matches_every_page(app, &term, &with, &[LIVE_STAGE_ID], 10).await;
        let visibility = app
            .state
            .items()
            .search_visibility(&with, &[LIVE_STAGE_ID])
            .await;
        let page = app
            .state
            .search()
            .search_unrecorded(&term, &visibility, 10, 0)
            .await
            .expect("search");
        assert_eq!(
            page.total, 2,
            "with the permission, the draft is a candidate alongside the published item"
        );
    });
}

/// A published item on an Internal-visibility stage is left out of search for
/// anyone but an administrator, even when its stage is explicitly requested
/// (the shape a `use stages` holder who selected that stage would produce) —
/// stated as a limit in the docs, not a `tap_item_access` decision search makes
/// per row.
#[test]
fn internal_stage_item_is_left_out_for_a_non_admin_viewer() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let internal = common::create_test_stage(app, "b248intstage").await;

        let term = format!("zqxb248int{}", Uuid::now_v7().simple());
        make_item(app, &format!("{term} live"), Uuid::nil(), 1, LIVE_STAGE_ID).await;
        make_item(app, &format!("{term} internal"), Uuid::nil(), 1, internal).await;

        let (reader, _) = common::user_holding(app, "b248intreader", &[]).await;
        let viewer = UserContext::authenticated(reader, vec!["access content".to_string()]);
        let requested = [internal, LIVE_STAGE_ID];

        assert_total_matches_every_page(app, &term, &viewer, &requested, 10).await;
        let visibility = app
            .state
            .items()
            .search_visibility(&viewer, &requested)
            .await;
        assert_eq!(
            visibility.stage_ids,
            vec![LIVE_STAGE_ID],
            "the internal stage must be dropped from the predicate for a non-admin viewer"
        );
        let page = app
            .state
            .search()
            .search_unrecorded(&term, &visibility, 10, 0)
            .await
            .expect("search");
        assert_eq!(
            page.total, 1,
            "the internal-stage item must not be a candidate at all"
        );
    });
}

/// An administrator bypasses every rule above: every status, on every
/// requested stage, including another author's unpublished item and the
/// internal-stage item.
#[test]
fn administrator_sees_every_status_on_every_requested_stage() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let internal = common::create_test_stage(app, "b248adminint").await;
        let (someone_else, _) = common::user_holding(app, "b248adminauthor", &[]).await;

        let term = format!("zqxb248admin{}", Uuid::now_v7().simple());
        make_item(
            app,
            &format!("{term} published"),
            Uuid::nil(),
            1,
            LIVE_STAGE_ID,
        )
        .await;
        make_item(
            app,
            &format!("{term} draft"),
            someone_else,
            0,
            LIVE_STAGE_ID,
        )
        .await;
        make_item(app, &format!("{term} internal"), Uuid::nil(), 1, internal).await;

        let requested = [internal, LIVE_STAGE_ID];
        assert_total_matches_every_page(app, &term, &admin(), &requested, 2).await;
        let visibility = app
            .state
            .items()
            .search_visibility(&admin(), &requested)
            .await;
        assert!(
            visibility.admin_bypass,
            "an administrator's visibility must bypass status and own-author entirely"
        );
        let page = app
            .state
            .search()
            .search_unrecorded(&term, &visibility, 10, 0)
            .await
            .expect("search");
        assert_eq!(
            page.total, 3,
            "an administrator is a candidate for all three: published, another \
             author's draft, and the internal-stage item"
        );
    });
}
