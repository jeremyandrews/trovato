#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B160: the server search takes any query a visitor types.
//!
//! The query used to be turned into `to_tsquery` text by hand, each word with
//! `:*` appended, so a query holding `&`, `<b>` or a stray quote was tsquery
//! syntax and the page answered 500. It is now parsed by
//! `websearch_to_tsquery`, which reads punctuation as punctuation: every word
//! must match, "a quoted phrase" is a phrase.
//!
//! Also here: a query with no word left to search for (`a`, `&`) says so; the
//! results are written once, not twice; and searching asks for `search content`.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::role::well_known::ANONYMOUS_ROLE_ID;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// The permission test removes a grant from the anonymous role; every other
/// test here searches as that role.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// A word no other item in the shared database holds.
fn unique_term(prefix: &str) -> String {
    let id = Uuid::now_v7().simple().to_string();
    format!("{prefix}{}", &id[id.len() - 12..])
}

/// Publish a conference item, deleted again when the test ends.
async fn publish(title: &str, body: &str) {
    let app = shared_app().await;
    app.ensure_conference_type().await;
    let item = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: "conference".to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({ "field_body": { "value": body } })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("b160 search query test".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create");
    let items = std::sync::Arc::clone(app.state.items());
    common::defer_cleanup(move || async move {
        let _ = items.delete(item.id, &admin()).await;
    });
}

/// GET `uri` as an anonymous visitor: the status and the body.
async fn get(uri: &str) -> (StatusCode, String) {
    let app = shared_app().await;
    let response = app
        .request(Request::get(uri).body(Body::empty()).unwrap())
        .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn search_uri(query: &str) -> String {
    format!("/search?q={}", urlencoding::encode(query))
}

/// How many results the page lists.
fn result_count(html: &str) -> usize {
    html.matches("<article class=\"search-result\">").count()
}

/// The reproduction: `&`, markup, quotes, an apostrophe and non-ASCII letters
/// in the query all answer 200 and find the item, rendered safely.
#[test]
fn special_characters_in_a_query_search_instead_of_failing() {
    run_test(async {
        let _serial = SERIAL.lock().await;
        let term = unique_term("zqnoank");
        publish(
            &format!("Noank & Mystic {term}"),
            &format!("<p>Coffee at the café by the {term} harbour, couldn\u{2019}t be better.</p>"),
        )
        .await;

        for query in [
            format!("noank & {term}"),
            format!("<b>{term}"),
            format!("\"mystic {term}\""),
            format!("{term}'s"),
            format!("café {term}"),
            format!("{term} <script>alert(1)</script>"),
        ] {
            let (status, html) = get(&search_uri(&query)).await;
            assert_eq!(status, StatusCode::OK, "{query:?} answered {status}");
            assert_eq!(
                result_count(&html),
                1,
                "{query:?} should find the one item, listed once:\n{html}"
            );
            assert!(
                !html.contains("<script>alert(1)") && !html.contains("<b>zq"),
                "{query:?} must not reach the page as markup"
            );
        }

        // The query is shown back to the visitor escaped once.
        let (_, html) = get(&search_uri(&format!("<b>{term}"))).await;
        assert!(
            html.contains(&format!("&lt;b&gt;{term}")),
            "the query, escaped once, in the summary"
        );
        assert!(!html.contains("&amp;lt;b"), "the query escaped twice");

        // The JSON search takes the same query.
        let (status, body) = get(&format!(
            "/api/v1/search?q={}",
            urlencoding::encode(&format!("noank & {term}"))
        ))
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["data"].as_array().map(Vec::len), Some(1), "{json}");
    });
}

/// A word nothing holds is "no results"; a query left with no word to search
/// for says that instead.
#[test]
fn a_miss_and_a_query_with_no_searchable_word_say_which() {
    run_test(async {
        let _serial = SERIAL.lock().await;
        let (status, html) = get(&search_uri(&unique_term("zqzzqxjv"))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("No results found"), "{html}");
        assert!(!html.contains("search-too-short"));

        for query in ["a", "&", "it's", "\"\""] {
            let (status, html) = get(&search_uri(query)).await;
            assert_eq!(status, StatusCode::OK, "{query:?} answered {status}");
            assert!(
                html.contains("class=\"search-too-short"),
                "{query:?} should say it has no word to search for:\n{html}"
            );
            assert_eq!(result_count(&html), 0);
        }
    });
}

/// Searching asks for `search content`: without it the page is a 403.
#[test]
fn search_asks_for_search_content() {
    run_test(async {
        let _serial = SERIAL.lock().await;
        let app = shared_app().await;

        let granted = trovato_kernel::models::Role::get_permissions(&app.db, ANONYMOUS_ROLE_ID)
            .await
            .unwrap()
            .iter()
            .any(|p| p == "search content");
        assert!(
            granted,
            "the migration grants `search content` to the anonymous role, which holds `access content`"
        );

        app.state
            .roles()
            .remove_permission(ANONYMOUS_ROLE_ID, "search content")
            .await
            .unwrap();
        let roles = std::sync::Arc::clone(app.state.roles());
        common::defer_cleanup(move || async move {
            let _ = roles
                .add_permission(ANONYMOUS_ROLE_ID, "search content")
                .await;
        });
        common::settle_cache_generations().await;
        let (page, _) = get("/search?q=anything").await;
        let (api, _) = get("/api/v1/search?q=anything").await;
        app.state
            .roles()
            .add_permission(ANONYMOUS_ROLE_ID, "search content")
            .await
            .unwrap();
        common::settle_cache_generations().await;

        assert_eq!(
            page,
            StatusCode::FORBIDDEN,
            "the search page without the permission"
        );
        assert_eq!(
            api,
            StatusCode::UNAUTHORIZED,
            "the JSON search refuses an anonymous caller without it"
        );
        let (again, _) = get("/search?q=anything").await;
        assert_eq!(again, StatusCode::OK, "with the permission back");
    });
}
