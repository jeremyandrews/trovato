#![allow(clippy::unwrap_used, clippy::expect_used)]
//! FR-8 Story 3.7 — search result access enforcement.
//!
//! `SearchService::search` applies only a coarse `status`/`author`/`stage`
//! filter and builds `ts_headline` snippets from raw `field_body`, with no
//! field-level access. Story 3.7 routes results through the shared seam
//! (`ItemService::filter_search_results`): a restricted item is absent from
//! results entirely, and the snippet is redacted when the viewer may not see
//! its `field_body`.
//!
//! Field-level *redaction on denial* rides the same `field_access_decisions`
//! seam validated end-to-end through the reference plugin in Story 3.8; the full
//! `TestApp` here loads no field-access plugin, so — as `keeps_snippet_fail_open`
//! pins — a governed field defaults visible and the snippet is preserved. These
//! tests cover the item-level tier and the fail-open field pass at the real
//! search boundary.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use common::{run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn anon_with_access() -> UserContext {
    let mut ctx = UserContext::anonymous();
    ctx.permissions = vec!["access content".to_string()];
    ctx
}

async fn make_item(app: &common::TestApp, admin: &UserContext, title: &str, status: i16) -> Uuid {
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
                fields: Some(serde_json::json!({
                    "field_body": { "value": format!("{title} body detail") }
                })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("3.7 search test".to_string()),
            },
            admin,
        )
        .await
        .expect("create")
        .id
}

/// Run search then route results through the seam as the given viewer.
async fn search_as(
    app: &common::TestApp,
    term: &str,
    viewer: &UserContext,
) -> Vec<trovato_kernel::search::SearchResult> {
    let raw = app
        .state
        .search()
        .search(term, &[LIVE_STAGE_ID], None, 50, 0)
        .await
        .expect("search");
    app.state
        .items()
        .filter_search_results(raw.results, viewer)
        .await
}

/// AC-1/AC-3 — a restricted (unpublished) item is absent from search results
/// entirely for an anonymous viewer; the published one is present.
#[test]
fn search_excludes_restricted_item() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let admin = UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()]);

        let term = format!("zqxsearch{}", Uuid::now_v7().simple());
        let published = make_item(app, &admin, &format!("{term} public"), 1).await;
        let draft = make_item(app, &admin, &format!("{term} draft"), 0).await;

        let results = search_as(app, &term, &anon_with_access()).await;
        let ids: Vec<Uuid> = results.iter().map(|r| r.id).collect();
        assert!(
            ids.contains(&published),
            "published item must be searchable"
        );
        assert!(
            !ids.contains(&draft),
            "restricted (unpublished) item must be absent from results, got {ids:?}"
        );
    });
}

/// Admin sees both the published and the unpublished item in search results.
#[test]
fn search_includes_restricted_item_for_admin() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let admin = UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()]);

        let term = format!("zqxadmin{}", Uuid::now_v7().simple());
        let published = make_item(app, &admin, &format!("{term} public"), 1).await;
        let draft = make_item(app, &admin, &format!("{term} draft"), 0).await;

        // Admin passes user_id so the coarse SQL includes own/all drafts; the
        // seam's admin bypass keeps both.
        let raw = app
            .state
            .search()
            .search(&term, &[LIVE_STAGE_ID], Some(admin.id), 50, 0)
            .await
            .expect("search");
        let results = app
            .state
            .items()
            .filter_search_results(raw.results, &admin)
            .await;
        let ids: Vec<Uuid> = results.iter().map(|r| r.id).collect();
        assert!(ids.contains(&published));
        assert!(ids.contains(&draft), "admin sees the unpublished item too");
    });
}

/// Fail-open field pass — with no field-access plugin, a governed field
/// (`field_body`) defaults visible, so the snippet is preserved (the field pass
/// runs without over-redacting). Denial redaction is validated via the
/// `field_access_decisions` seam (Story 3.8).
#[test]
fn search_keeps_snippet_fail_open() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let admin = UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()]);

        let term = format!("zqxsnip{}", Uuid::now_v7().simple());
        make_item(app, &admin, &format!("{term} public"), 1).await;

        let results = search_as(app, &term, &anon_with_access()).await;
        let hit = results
            .iter()
            .find(|r| r.title.contains(&term))
            .expect("the published item is a search hit");
        assert!(
            hit.snippet.is_some(),
            "fail-open: a governed field's snippet is preserved with no field-access plugin"
        );
    });
}

/// XSS-3 regression (FR-6 audit): `ts_headline` builds the search snippet over
/// the stored title/body, which is rendered `| safe`. HTML stored in the source
/// (e.g. `<img src=x onerror=alert(1)>`) must be escaped in the snippet — only
/// the `<mark>` highlight may be raw HTML. The fix HTML-escapes the source
/// columns before `ts_headline`.
#[test]
fn search_snippet_escapes_html_in_source() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let admin = UserContext::authenticated(Uuid::nil(), vec!["administer site".to_string()]);

        let term = format!("zqxxss{}", Uuid::now_v7().simple());
        // Store an XSS payload in the body, right next to the searchable term so
        // it lands inside the ts_headline window.
        app.state
            .items()
            .create(
                CreateItem {
                    item_type: "conference".to_string(),
                    title: format!("{term} keynote"),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({
                        "field_body": {
                            "value": format!(
                                "{term} session <img src=x onerror=alert(1)> with further \
                                 descriptive words padding out the headline window for readers"
                            )
                        }
                    })),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("xss-3 test".to_string()),
                },
                &admin,
            )
            .await
            .expect("create");

        let raw = app
            .state
            .search()
            .search(&term, &[LIVE_STAGE_ID], None, 50, 0)
            .await
            .expect("search");
        let hit = raw
            .results
            .into_iter()
            .find(|r| r.title.contains(&term))
            .expect("the published item is a search hit");
        let snippet = hit.snippet.expect("snippet present");

        assert!(
            !snippet.contains("<img"),
            "raw <img must not survive into the snippet: {snippet}"
        );
        assert!(
            snippet.contains("&lt;img"),
            "the stored markup must be HTML-escaped in the snippet: {snippet}"
        );
    });
}
