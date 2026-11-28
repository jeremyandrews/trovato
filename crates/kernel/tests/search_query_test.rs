#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B160: the server search takes any query a visitor types.
//!
//! The query used to be turned into `to_tsquery` text by hand, each word with
//! `:*` appended, so a query holding `&`, `<b>` or a stray quote was tsquery
//! syntax and the page answered 500. B160 moved to `websearch_to_tsquery`,
//! which reads punctuation as punctuation (every word must match, "a quoted
//! phrase" is a phrase) but, having no prefix operator, lost prefix matching
//! as a side effect.
//!
//! B307 restores it: `SearchService` builds its own `to_tsquery` operand
//! ([`trovato_kernel::search`]'s private `build_prefix_tsquery`), quoting
//! every lexeme so no visitor character is ever tsquery syntax, same as
//! `websearch_to_tsquery` guaranteed, while appending `:*` to a bare word so
//! `kernel` finds `kerneltrap`. A quoted phrase still matches exactly, with
//! no prefix.
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
        common::hold_until_cleanup(SERIAL.lock().await);
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
            format!("{term}\""),
            format!("{term}:*"),
            format!("{term}!"),
            format!("{term}("),
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
        common::hold_until_cleanup(SERIAL.lock().await);
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
        common::hold_until_cleanup(SERIAL.lock().await);
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

/// B307: a bare word matches as a prefix again, the way Drupal's search did
/// before B160 (`kernel` finds `kerneltrap` and `kernels`), while a suffix of
/// the same stored word (not a prefix) still does not, and a quoted word
/// matches only exactly, with no prefix.
#[test]
fn a_bare_word_matches_as_a_prefix_again() {
    run_test(async {
        common::hold_until_cleanup(SERIAL.lock().await);
        let stem = unique_term("zqkernel");
        let compound = format!("{stem}trap"); // the "kerneltrap" shape
        let plural = format!("{stem}s"); // the "kernels" shape
        let other = unique_term("zqfreebsd"); // unrelated to `stem`

        publish(&format!("Compound {compound}"), "body").await;
        publish(&format!("Plural-shaped {plural}"), "body").await;
        publish(&format!("Unrelated {other}"), "body").await;

        // The bare stem, a true prefix of both, finds both by prefix.
        let (status, html) = get(&search_uri(&stem)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            result_count(&html),
            2,
            "{stem:?} should find the compound and the plural-shaped word by prefix:\n{html}"
        );

        // A prefix of the unrelated word finds only it: `free` must not also
        // match `freebsd` through the other two items.
        let other_prefix = &other[..other.len() - 4];
        let (_, html) = get(&search_uri(other_prefix)).await;
        assert_eq!(
            result_count(&html),
            1,
            "{other_prefix:?} should find only the unrelated item:\n{html}"
        );

        // A suffix of the same stored word (not a prefix) does not match:
        // prefix matching is anchored at the start of the word.
        let suffix_only = &other[other.len() - 4..];
        let (_, html) = get(&search_uri(suffix_only)).await;
        assert_eq!(
            result_count(&html),
            0,
            "{suffix_only:?}, a suffix, must not match as a prefix:\n{html}"
        );

        // Quoted, the bare stem is an exact phrase: it must not prefix-match
        // the compound or the plural-shaped word.
        let (_, html) = get(&search_uri(&format!("\"{stem}\""))).await;
        assert_eq!(
            result_count(&html),
            0,
            "a quoted word must match exactly, not as a prefix:\n{html}"
        );

        // Quoted, the full compound word still matches (an exact phrase of
        // one word is just that word).
        let (_, html) = get(&search_uri(&format!("\"{compound}\""))).await;
        assert_eq!(
            result_count(&html),
            1,
            "the full word, quoted, still matches exactly:\n{html}"
        );
    });
}

/// B307 preserves what B160 added along with prefix matching: `-word`
/// excludes, and `a or b` matches either, both still with prefix matching on
/// the words that are not excluded.
#[test]
fn exclusion_and_or_still_work_with_prefix_matching() {
    run_test(async {
        common::hold_until_cleanup(SERIAL.lock().await);
        let stem = unique_term("zqeither");
        let first = format!("{stem}alpha");
        let second = format!("{stem}beta");

        publish(&format!("First {first}"), "body").await;
        publish(&format!("Second {second}"), "body").await;

        // The shared prefix alone finds both.
        let (_, html) = get(&search_uri(&stem)).await;
        assert_eq!(result_count(&html), 2, "{stem:?} should find both:\n{html}");

        // Excluding the second by its own prefix leaves only the first.
        let (_, html) = get(&search_uri(&format!("{stem} -{second}"))).await;
        assert_eq!(
            result_count(&html),
            1,
            "excluding the second should leave only the first:\n{html}"
        );

        // `a or b`, each a prefix of a different item, matches either.
        let (status, html) = get(&search_uri(&format!("{first} or {second}"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            result_count(&html),
            2,
            "`a or b` should match either item:\n{html}"
        );
    });
}
