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
        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);

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
        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);

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
        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);

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
        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);

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

// =============================================================================
// The two read paths that formatted item fields for somebody else to read:
// the AI chat's RAG context, and the Pagefind static index.
//
// Both went straight from SQL to text with no access decision at either tier,
// so a field `tap_field_access` denies the viewer was written into the model
// prompt, and into a static index any visitor can download. These run the real
// reference plugin (`plugins/trovato_field_access_ref`) over the live test pool,
// because fail-open is the shared app's answer for every governed field and a
// test needs a real denial.
// =============================================================================

/// The reference plugin's rules deny `ssn` on type `person` without `view pii`.
const PII_TYPE: &str = "person";

async fn ensure_person_type(app: &common::TestApp) {
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'Person', 'Field-access fixture', true, 'Name', 'seam_test', '{\"fields\": []}'::jsonb) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(PII_TYPE)
    .execute(&app.db)
    .await
    .expect("seed the person type");

    app.state
        .content_types()
        .create(
            PII_TYPE,
            "Person",
            Some("Field-access fixture"),
            serde_json::json!({ "fields": [] }),
        )
        .await
        .ok();

    // Both fields are searchable, so the Pagefind exporter reaches for both and
    // the field tier is what decides between them.
    for field in ["ssn", "bio"] {
        sqlx::query(
            "INSERT INTO search_field_config (id, bundle, field_name, weight) \
             VALUES ($1, $2, $3, 'C') ON CONFLICT (bundle, field_name) DO NOTHING",
        )
        .bind(Uuid::now_v7())
        .bind(PII_TYPE)
        .bind(field)
        .execute(&app.db)
        .await
        .expect("configure a searchable field");
    }
}

/// A published person carrying one governed field and one ungoverned one.
async fn make_person(app: &common::TestApp, name: &str, ssn: &str, bio: &str) -> Uuid {
    let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
    app.state
        .items()
        .create(
            CreateItem {
                item_type: PII_TYPE.to_string(),
                title: name.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({ "ssn": ssn, "bio": bio })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("field-access read-path test".to_string()),
            },
            &admin,
        )
        .await
        .expect("create")
        .id
}

fn viewer(perms: &[&str]) -> UserContext {
    UserContext::authenticated(
        Uuid::now_v7(),
        perms.iter().map(|s| (*s).to_string()).collect(),
    )
}

/// Problem 5 — the chat's RAG context is built from the fields the viewer may
/// see, through both tiers of the shared seam.
#[test]
fn the_chat_rag_context_drops_fields_the_viewer_may_not_see() {
    run_test(async {
        let app = shared_app().await;
        ensure_person_type(app).await;
        let items = common::item_service_with_ref_plugin(app.db.clone());

        let tag = format!("zqxrag{}", Uuid::now_v7().simple());
        let ssn = format!("ssn-{tag}");
        let bio = format!("bio-{tag} writes about the kernel");
        let person = make_person(app, &format!("{tag} person"), &ssn, &bio).await;
        assert_ne!(person, Uuid::nil());

        let config = trovato_kernel::services::ai_chat::ChatConfig {
            rag_enabled: true,
            rag_max_results: 10,
            rag_min_score: 0.0,
            ..Default::default()
        };

        // A reader with no `view pii`.
        let context = app
            .state
            .ai_chat()
            .search_for_context(&tag, &config, &viewer(&["access content"]), &items)
            .await;
        assert!(
            context.contains(&bio),
            "the fields this reader may see must still reach the model, got {context}"
        );
        assert!(
            !context.contains(&ssn),
            "a denied field must not be written into the model prompt, got {context}"
        );

        // A reader who holds it sees it.
        let context = app
            .state
            .ai_chat()
            .search_for_context(
                &tag,
                &config,
                &viewer(&["access content", "view pii"]),
                &items,
            )
            .await;
        assert!(
            context.contains(&ssn),
            "a holder of `view pii` must still get the field, got {context}"
        );
    });
}

/// Problem 5, item tier — a hit the viewer may not see at all is dropped, and
/// its snippet is not used as a fallback.
///
/// The draft is the viewer's **own**, which is the case the coarse SQL cannot
/// answer: its `status = 1 OR author_id = $user` arm puts an author's own drafts
/// in the result set, and whether they may read one back is a question only
/// `check_access` answers — `view own content`, which this viewer does not hold.
#[test]
fn the_chat_rag_context_drops_items_the_viewer_may_not_see() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let items = common::item_service_with_ref_plugin(app.db.clone());
        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);

        // A real user, because an item's author is a foreign key.
        let (author, _) = common::user_holding(app, "ragowner", &[]).await;
        let reader = UserContext::authenticated(author, vec!["access content".to_string()]);

        let tag = format!("zqxragitem{}", Uuid::now_v7().simple());
        let published = make_item(app, &admin, &format!("{tag} public"), 1).await;
        let draft = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: "conference".to_string(),
                    title: format!("{tag} draft"),
                    author_id: author,
                    status: Some(0),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({
                        "field_body": { "value": format!("{tag} draft body detail") }
                    })),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("rag item tier test".to_string()),
                },
                &admin,
            )
            .await
            .expect("create")
            .id;
        assert_ne!(published, draft);

        let config = trovato_kernel::services::ai_chat::ChatConfig {
            rag_enabled: true,
            rag_max_results: 10,
            rag_min_score: 0.0,
            ..Default::default()
        };

        let context = app
            .state
            .ai_chat()
            .search_for_context(&tag, &config, &reader, &items)
            .await;
        assert!(
            context.contains(&format!("{tag} public")),
            "the published item must still be context, got {context}"
        );
        assert!(
            !context.contains("draft"),
            "an item this reader cannot see must not reach the model at all, got {context}"
        );
    });
}

/// Problem 9 — the static Pagefind index is built as the anonymous visitor, who
/// is who downloads it.
#[test]
fn the_pagefind_export_is_built_as_the_anonymous_visitor() {
    run_test(async {
        let app = shared_app().await;
        ensure_person_type(app).await;
        let items = common::item_service_with_ref_plugin(app.db.clone());

        let tag = format!("zqxpf{}", Uuid::now_v7().simple());
        let ssn = format!("ssn-{tag}");
        let bio = format!("bio-{tag} writes about the kernel");
        let person = make_person(app, &format!("{tag} person"), &ssn, &bio).await;

        let docs = trovato_kernel::cron::indexable_documents(&app.db, &items)
            .await
            .expect("build the export list");

        let doc = docs
            .iter()
            .find(|d| d.id == person)
            .expect("a published live item is still exported");
        assert!(
            doc.body.contains(&bio),
            "the fields an anonymous visitor may see are still indexed, got {}",
            doc.body
        );
        assert!(
            !doc.body.contains(&ssn),
            "a field denied to anonymous visitors must not be indexed, got {}",
            doc.body
        );
        assert!(
            !doc.description.contains(&ssn) && !doc.location.contains(&ssn),
            "nor may it reach a meta value"
        );

        // And an unpublished item is not in the export at all, as before.
        let draft = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: PII_TYPE.to_string(),
                    title: format!("{tag} draft person"),
                    author_id: Uuid::nil(),
                    status: Some(0),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({ "bio": "unpublished" })),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("pagefind test".to_string()),
                },
                &UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]),
            )
            .await
            .expect("create")
            .id;
        let docs = trovato_kernel::cron::indexable_documents(&app.db, &items)
            .await
            .expect("build the export list");
        assert!(
            !docs.iter().any(|d| d.id == draft),
            "an unpublished item is not exported"
        );
    });
}
