#![allow(clippy::unwrap_used, clippy::expect_used)]
//! B123: a search snippet is escaped exactly once.
//!
//! The snippet is built from the stored title and body with the tags stripped
//! from that source, highlighted by `ts_headline` with fixed marker tokens,
//! entity-decoded, escaped once, and only then are the markers turned into
//! `<mark>`. The results page prints it `| safe` on the strength of that.
//!
//! Before B123 the source was escaped in SQL, then the whole snippet escaped
//! again in the route, so `&amp;` in an HTML body reached the browser as
//! `&amp;amp;amp;` and a `<p>` showed as the literal text `&lt;p&gt;`.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// A term no other item in the shared database holds.
fn unique_term(prefix: &str) -> String {
    let id = Uuid::now_v7().simple().to_string();
    format!("{prefix}{}", &id[id.len() - 12..])
}

/// Create a published conference item with this title and body.
async fn publish(title: &str, body: &str) {
    publish_fields(
        title,
        serde_json::json!({ "field_body": { "value": body } }),
    )
    .await;
}

/// Create a published conference item with this title and these fields.
async fn publish_fields(title: &str, fields: serde_json::Value) {
    let app = shared_app().await;
    app.ensure_conference_type().await;
    let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
    app.state
        .items()
        .create(
            CreateItem {
                item_type: "conference".to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(fields),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("b123 snippet test".to_string()),
            },
            &admin,
        )
        .await
        .expect("create");
}

/// GET `/search?q=…` and return the inner HTML of the one result's snippet.
async fn rendered_snippet(query: &str) -> String {
    let app = shared_app().await;
    let uri = format!("/search?q={}", urlencoding::encode(query));
    let response = app
        .request(Request::get(&uri).body(Body::empty()).unwrap())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    let html = String::from_utf8_lossy(&bytes).into_owned();

    // The page writes its results once (B160): exactly one snippet, which also
    // proves the unique term matched a single item.
    let open = "<div class=\"result-snippet\">";
    let snippets: Vec<&str> = html
        .match_indices(open)
        .map(|(i, _)| {
            let start = i + open.len();
            let end = start + html[start..].find("</div>").expect("snippet closes");
            html[start..end].trim()
        })
        .collect();
    assert!(
        !snippets.is_empty(),
        "no snippet on the results page for {query:?}:\n{html}"
    );
    assert!(
        snippets.len() == 1,
        "the query {query:?} should match exactly one item: {snippets:?}"
    );
    snippets[0].to_string()
}

/// The reproduction: an HTML body with `<p>`, `&amp;` and quotes shows its text
/// escaped once, the term highlighted, no tag visible as text.
#[test]
fn html_body_snippet_is_escaped_once_with_the_term_highlighted() {
    run_test(async {
        let term = unique_term("zqhtml");
        publish(
            "Cartoon review",
            &format!("<p>Tom &amp; Jerry said \"{term}\" before lunch.</p>"),
        )
        .await;

        let snippet = rendered_snippet(&term).await;

        assert!(
            snippet.contains(&format!(
                "Tom &amp; Jerry said &quot;<mark>{term}</mark>&quot; before lunch."
            )),
            "the body text, escaped once, term highlighted; got: {snippet}"
        );
        for wrong in ["&amp;amp;", "&amp;lt;", "&lt;p&gt;", "<p>", "&amp;quot;"] {
            assert!(
                !snippet.contains(wrong),
                "{wrong:?} must not appear in the snippet; got: {snippet}"
            );
        }
    });
}

/// A stored `<script>` never reaches the page as markup.
#[test]
fn script_in_the_body_renders_inert() {
    run_test(async {
        let term = unique_term("zqscript");
        publish(
            "Script holder",
            &format!("{term} first <script>alert(1)</script> then more words"),
        )
        .await;

        let snippet = rendered_snippet(&term).await;

        assert!(!snippet.contains("<script"), "got: {snippet}");
        assert!(
            snippet.contains(&format!("<mark>{term}</mark>")),
            "got: {snippet}"
        );
    });
}

/// A stored `<mark>` is not a highlight: only the kernel's markers become one.
#[test]
fn a_stored_mark_tag_is_not_a_highlight() {
    run_test(async {
        let term = unique_term("zqmark");
        publish(
            "Mark holder",
            &format!(
                "{term} and <mark>planted</mark> and &lt;mark&gt;escaped&lt;/mark&gt; \
                 and \u{E000}forged\u{E001} too"
            ),
        )
        .await;

        let snippet = rendered_snippet(&term).await;

        assert_eq!(
            snippet.matches("<mark>").count(),
            1,
            "only the searched term is highlighted; got: {snippet}"
        );
        assert!(!snippet.contains("<mark>planted"), "got: {snippet}");
        assert!(
            snippet.contains("&lt;mark&gt;escaped&lt;/mark&gt;"),
            "text that reads <mark> shows as that text; got: {snippet}"
        );
    });
}

/// A plain text body (no markup) shows as typed.
#[test]
fn plain_text_body_is_unchanged() {
    run_test(async {
        let term = unique_term("zqplain");
        publish(
            "Plain holder",
            &format!("Just {term} words, with a comma and 3 < 4 maths."),
        )
        .await;

        let snippet = rendered_snippet(&term).await;

        assert!(
            snippet.contains(&format!(
                "Just <mark>{term}</mark> words, with a comma and 3 &lt; 4 maths."
            )),
            "got: {snippet}"
        );
    });
}

/// Every term of a multi-word query is highlighted.
#[test]
fn every_term_of_a_multi_word_query_is_highlighted() {
    run_test(async {
        let first = unique_term("zqfirst");
        let second = unique_term("zqsecond");
        publish(
            "Two terms",
            &format!("<p>The {first} sits here</p><p>and the {second} sits there.</p>"),
        )
        .await;

        let snippet = rendered_snippet(&format!("{first} {second}")).await;

        assert!(
            snippet.contains(&format!("<mark>{first}</mark>")),
            "got: {snippet}"
        );
        assert!(
            snippet.contains(&format!("<mark>{second}</mark>")),
            "got: {snippet}"
        );
        assert!(!snippet.contains("&lt;p"), "got: {snippet}");
    });
}

/// A page builder body is a JSON document, not text: the snippet never prints
/// it. `fields->>'field_body'` on that object returned its JSON text, so every
/// page builder item's snippet read `{"root": {"props": {}}, "content": …`.
#[test]
fn page_builder_body_is_not_printed_as_json() {
    run_test(async {
        let term = unique_term("zqpagebuilder");
        publish_fields(
            &format!("Composed {term} page"),
            serde_json::json!({
                "field_body": {
                    "root": { "props": {} },
                    "content": [
                        { "type": "TextBlock", "props": { "content": "Body words here." } }
                    ]
                }
            }),
        )
        .await;

        let snippet = rendered_snippet(&term).await;

        assert!(
            snippet.contains(&format!("<mark>{term}</mark>")),
            "the title term is highlighted; got: {snippet}"
        );
        for wrong in ["root", "props", "content", "TextBlock", "{", "&quot;"] {
            assert!(
                !snippet.contains(wrong),
                "{wrong:?} from the page builder JSON must not appear; got: {snippet}"
            );
        }
    });
}

/// A body stored as a bare JSON string is still text, as before.
#[test]
fn bare_string_body_is_still_text() {
    run_test(async {
        let term = unique_term("zqbare");
        publish_fields(
            "Bare holder",
            serde_json::json!({ "field_body": format!("<p>Stored bare {term} words.</p>") }),
        )
        .await;

        let snippet = rendered_snippet(&term).await;

        assert!(
            snippet.contains(&format!("Stored bare <mark>{term}</mark> words.")),
            "got: {snippet}"
        );
    });
}
