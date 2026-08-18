//! RSS 2.0 feeds for gather queries.
//!
//! A gather query declares a feed in its display config
//! ([`GatherFeed`](crate::gather::types::GatherFeed)) and gets an RSS document
//! at the path it names. Routes are built once at startup, the same shape
//! [`crate::routes::gather_routes::build_gather_route_router`] uses for gather
//! route aliases.
//!
//! # Why this is kernel rather than a plugin
//!
//! Feeds are a feature, and the kernel-minimality rule sends features to
//! plugins. The blocker is that a feed is a rendering of a *query result*, and
//! query execution is kernel infrastructure: `execute_query_only` applies the
//! stage filter, the access filter and the D-26 over-fetch bounds for a
//! specific viewer. Plugin space has no seam onto it — the `item-api` host
//! interface offers `query-items`, which is an unordered, unfiltered
//! `SELECT ... LIMIT 100` with no viewer, so a plugin-built feed would leak
//! whatever the access filter exists to withhold. The plugin-facing seam that
//! would fix that is plugin-contract surface, and the contract is frozen before
//! 1.0.
//!
//! The precedent is `crate::routes::sitemap`, which serves `sitemap.xml` and
//! `robots.txt` from the kernel for the same reason.
//!
//! # What a feed shows
//!
//! Whatever the viewer's own execution of the query returns. A feed URL fetched
//! by an anonymous aggregator is an anonymous execution, so it carries exactly
//! the rows an anonymous visitor would see on the query's page.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use tower_sessions::Session;

use crate::gather::types::GatherQuery;
use crate::models::UrlAlias;
use crate::models::stage::LIVE_STAGE_ID;
use crate::routes::gather::ExecuteParams;
use crate::state::AppState;

/// Largest number of entries a feed will carry, whatever its config asks for.
///
/// A feed is fetched unauthenticated and on a timer by every aggregator
/// subscribed to it, so its cost is not bounded by human patience the way a page
/// is.
const MAX_FEED_ITEMS: u32 = 200;

/// One registered feed.
#[derive(Debug, Clone)]
struct FeedConfig {
    /// Gather query to execute.
    query_id: String,
    /// Feed path, as declared.
    path: String,
    /// `<channel><title>`.
    title: String,
    /// `<channel><description>`.
    description: String,
    /// Number of entries to carry.
    items: u32,
}

/// A feed as a template sees it, for autodiscovery link tags.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FeedLink {
    /// Path the feed is served at.
    pub path: String,
    /// Human-readable feed title.
    pub title: String,
}

/// Every feed declared by a gather query, for `<link rel="alternate">` tags.
///
/// Sorted by path so the head is stable between restarts rather than following
/// registry iteration order.
pub fn feed_links(queries: &[GatherQuery]) -> Vec<FeedLink> {
    let mut links: Vec<FeedLink> = queries
        .iter()
        .filter_map(|query| {
            let feed = query.display.feed.as_ref()?;
            if !is_valid_feed_path(&feed.path) {
                return None;
            }
            Some(FeedLink {
                path: feed.path.clone(),
                title: feed_title(query),
            })
        })
        .collect();
    links.sort_by(|a, b| a.path.cmp(&b.path));
    links.dedup_by(|a, b| a.path == b.path);
    links
}

/// Build a router serving every gather query that declares a feed.
///
/// Called once at startup after gather queries are loaded. Entries are skipped
/// with a warning when they are unusable rather than absent: a path that is not
/// absolute, or one that collides with a feed already registered — axum panics
/// on a route conflict, and a config entity must not be able to take the process
/// down by declaring a duplicate.
pub fn build_feed_router(queries: &[GatherQuery]) -> Router<AppState> {
    let mut router = Router::new();
    let mut registered: std::collections::HashSet<String> = std::collections::HashSet::new();

    for query in queries {
        let Some(feed) = query.display.feed.as_ref() else {
            continue;
        };

        if !is_valid_feed_path(&feed.path) {
            tracing::warn!(
                query_id = %query.query_id,
                path = %feed.path,
                "skipping feed with a path that is not absolute"
            );
            continue;
        }

        if !registered.insert(feed.path.clone()) {
            tracing::warn!(
                query_id = %query.query_id,
                path = %feed.path,
                "skipping duplicate feed path"
            );
            continue;
        }

        let config = Arc::new(FeedConfig {
            query_id: query.query_id.clone(),
            path: feed.path.clone(),
            title: feed_title(query),
            description: feed
                .description
                .clone()
                .or_else(|| query.description.clone())
                .unwrap_or_default(),
            items: feed.items.clamp(1, MAX_FEED_ITEMS),
        });

        tracing::info!(
            query_id = %query.query_id,
            path = %feed.path,
            "registering feed"
        );

        router = router.route(
            &feed.path,
            get(move |state: State<AppState>, session: Session| {
                let config = Arc::clone(&config);
                async move { serve_feed(state, session, &config).await }
            }),
        );
    }

    router
}

/// A feed path has to be absolute, and cannot be a path pattern: a feed is one
/// document at one address, and `{param}` in an axum route would capture
/// segments nothing here can fill.
fn is_valid_feed_path(path: &str) -> bool {
    path.starts_with('/') && !path.contains('{') && !path.contains('}')
}

/// The feed's title: its own, else the query's label.
fn feed_title(query: &GatherQuery) -> String {
    query
        .display
        .feed
        .as_ref()
        .and_then(|f| f.title.clone())
        .unwrap_or_else(|| query.label.clone())
}

/// Execute the query as this viewer and render the result as RSS 2.0.
async fn serve_feed(
    State(state): State<AppState>,
    session: Session,
    config: &FeedConfig,
) -> Response {
    let viewer = crate::routes::item::get_user_context(&session, &state).await;

    let params = ExecuteParams::new(1, LIVE_STAGE_ID.to_string(), HashMap::new());
    let result = match crate::routes::gather::execute_query_only(
        &state,
        &config.query_id,
        params,
        None,
        viewer,
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            tracing::error!(
                query_id = %config.query_id,
                error = %e,
                "failed to execute gather query for feed"
            );
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to build feed",
            )
                .into_response();
        }
    };

    let site_url = state.runtime().site_url.trim_end_matches('/').to_string();
    let rows: Vec<&serde_json::Value> = result.items.iter().take(config.items as usize).collect();
    let aliases = load_aliases(&state, &rows).await;

    let entries: Vec<String> = rows
        .iter()
        .map(|row| render_entry(row, &site_url, &aliases))
        .collect();

    let body = build_rss_feed(
        &config.title,
        &format!("{site_url}{}", config.path),
        &config.description,
        &entries,
    );

    (
        axum::http::StatusCode::OK,
        [("content-type", "application/rss+xml; charset=utf-8")],
        body,
    )
        .into_response()
}

/// Resolve the URL aliases for a page of rows in one query.
///
/// A feed of twenty entries would otherwise be twenty alias lookups.
async fn load_aliases(state: &AppState, rows: &[&serde_json::Value]) -> HashMap<String, String> {
    let sources: Vec<String> = rows
        .iter()
        .filter_map(|row| row_id(row))
        .map(|id| format!("/item/{id}"))
        .collect();

    if sources.is_empty() {
        return HashMap::new();
    }

    UrlAlias::canonical_aliases_for(state.db(), &sources, LIVE_STAGE_ID, "en")
        .await
        .unwrap_or_else(|e| {
            // A feed with `/item/{uuid}` links is worth more than no feed.
            tracing::warn!(error = %e, "failed to load URL aliases for feed");
            HashMap::new()
        })
}

/// The row's item id, whatever JSON shape the query's field config produced.
fn row_id(row: &serde_json::Value) -> Option<String> {
    row.get("id").map(|v| match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// Render one gather row as an RSS `<item>`.
fn render_entry(
    row: &serde_json::Value,
    site_url: &str,
    aliases: &HashMap<String, String>,
) -> String {
    let title = row
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled");

    let link = row_id(row)
        .map(|id| {
            let source = format!("/item/{id}");
            let path = aliases.get(&source).cloned().unwrap_or(source);
            format!("{site_url}{path}")
        })
        .unwrap_or_else(|| site_url.to_string());

    // `summary` is what the row templates show; the two field names after it are
    // the pair the rest of the kernel reads for a description.
    let description = ["summary", "field_description", "field_body"]
        .iter()
        .find_map(|key| row.get(*key).and_then(field_text))
        .unwrap_or_default();

    let pub_date = row
        .get("created")
        .and_then(|v| v.as_i64())
        .and_then(rfc822)
        .unwrap_or_default();

    build_rss_item(title, &link, &description, &pub_date)
}

/// Read a gather row value as text, accepting both the bare string and the
/// `{"value": …}` field shape.
fn field_text(value: &serde_json::Value) -> Option<String> {
    let text = value
        .get("value")
        .and_then(|v| v.as_str())
        .or_else(|| value.as_str())?;
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Format a Unix timestamp as RFC 822, which is what RSS 2.0 `<pubDate>` wants.
fn rfc822(timestamp: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(timestamp, 0).map(|dt| dt.to_rfc2822())
}

/// Build an RSS 2.0 `<item>` element.
///
/// Text content is XML-escaped; the description goes in a CDATA section because
/// it may legitimately carry markup, with any `]]>` in it split so the section
/// cannot be closed early.
fn build_rss_item(title: &str, link: &str, description: &str, pub_date: &str) -> String {
    let mut item = String::from("    <item>\n");
    // Infallible: writing to a String.
    let _ = write!(
        item,
        "      <title>{}</title>\n\
         \x20     <link>{}</link>\n\
         \x20     <guid isPermaLink=\"true\">{}</guid>\n",
        xml_escape(title),
        xml_escape(link),
        xml_escape(link),
    );

    if !description.is_empty() {
        let _ = writeln!(
            item,
            "      <description><![CDATA[{}]]></description>",
            cdata_escape(description)
        );
    }
    if !pub_date.is_empty() {
        let _ = writeln!(item, "      <pubDate>{}</pubDate>", xml_escape(pub_date));
    }

    item.push_str("    </item>\n");
    item
}

/// Wrap rendered entries in an RSS 2.0 envelope.
fn build_rss_feed(title: &str, link: &str, description: &str, items: &[String]) -> String {
    let mut feed = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\">\n\
         \x20 <channel>\n",
    );

    // Infallible: writing to a String.
    let _ = write!(
        feed,
        "    <title>{}</title>\n\
         \x20   <link>{}</link>\n\
         \x20   <description>{}</description>\n\
         \x20   <atom:link href=\"{}\" rel=\"self\" type=\"application/rss+xml\"/>\n",
        xml_escape(title),
        xml_escape(link),
        xml_escape(description),
        xml_escape(link),
    );

    for item in items {
        feed.push_str(item);
    }

    feed.push_str("  </channel>\n</rss>\n");
    feed
}

/// Escape the five XML predefined entities in text content.
fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Make a string safe inside a CDATA section.
///
/// A CDATA section ends at the first `]]>`, so content containing that sequence
/// would otherwise break out of it and inject markup into the document. The
/// sequence is split across two sections, which is the standard encoding and
/// preserves the text exactly.
fn cdata_escape(input: &str) -> String {
    input.replace("]]>", "]]]]><![CDATA[>")
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::gather::types::{GatherFeed, QueryDefinition, QueryDisplay};

    fn query(query_id: &str, label: &str, feed: Option<GatherFeed>) -> GatherQuery {
        GatherQuery {
            query_id: query_id.to_string(),
            label: label.to_string(),
            description: Some("A description".to_string()),
            definition: QueryDefinition::default(),
            display: QueryDisplay {
                feed,
                ..QueryDisplay::default()
            },
            plugin: "core".to_string(),
            created: 0,
            changed: 0,
        }
    }

    fn feed(path: &str) -> GatherFeed {
        GatherFeed {
            path: path.to_string(),
            title: None,
            description: None,
            items: 20,
        }
    }

    #[test]
    fn only_queries_declaring_a_feed_get_one() {
        let queries = vec![
            query("with", "With Feed", Some(feed("/rss/with.xml"))),
            query("without", "No Feed", None),
        ];

        let links = feed_links(&queries);

        assert_eq!(
            links,
            vec![FeedLink {
                path: "/rss/with.xml".to_string(),
                title: "With Feed".to_string(),
            }]
        );
    }

    #[test]
    fn a_feed_title_falls_back_to_the_query_label() {
        let mut with_title = feed("/rss/a.xml");
        with_title.title = Some("Custom Title".to_string());

        assert_eq!(
            feed_title(&query("a", "Label", Some(with_title))),
            "Custom Title"
        );
        assert_eq!(
            feed_title(&query("b", "Label", Some(feed("/rss/b.xml")))),
            "Label"
        );
    }

    /// A feed path is config, so a bad one must be skipped rather than
    /// registered — a relative path or a route pattern would panic axum or
    /// capture segments nothing can fill.
    #[test]
    fn an_unusable_feed_path_is_refused() {
        for path in ["rss/relative.xml", "", "/rss/{slug}.xml"] {
            assert!(!is_valid_feed_path(path), "{path} must be refused");
        }
        assert!(is_valid_feed_path("/rss/blog.xml"));
    }

    /// Two queries claiming one path would panic axum at startup.
    #[test]
    fn a_duplicate_feed_path_is_registered_once() {
        let queries = vec![
            query("first", "First", Some(feed("/rss/same.xml"))),
            query("second", "Second", Some(feed("/rss/same.xml"))),
        ];

        let links = feed_links(&queries);

        assert_eq!(links.len(), 1, "one path, one feed");
        // Building the router must not panic either.
        let _router = build_feed_router(&queries);
    }

    #[test]
    fn autodiscovery_links_are_ordered_by_path() {
        let queries = vec![
            query("b", "B", Some(feed("/rss/b.xml"))),
            query("a", "A", Some(feed("/rss/a.xml"))),
        ];

        let paths: Vec<String> = feed_links(&queries).into_iter().map(|f| f.path).collect();

        assert_eq!(paths, vec!["/rss/a.xml", "/rss/b.xml"]);
    }

    #[test]
    fn an_entry_carries_title_link_guid_and_date() {
        let row = serde_json::json!({
            "id": "01234567-89ab-7def-8123-456789abcdef",
            "title": "A Post",
            "summary": "What it is about.",
            "created": 1_700_000_000,
        });

        let entry = render_entry(&row, "https://example.com", &HashMap::new());

        assert!(entry.contains("<title>A Post</title>"), "{entry}");
        assert!(
            entry.contains(
                "<link>https://example.com/item/01234567-89ab-7def-8123-456789abcdef</link>"
            ),
            "{entry}"
        );
        assert!(entry.contains("<guid isPermaLink=\"true\">"), "{entry}");
        assert!(entry.contains("<![CDATA[What it is about.]]>"), "{entry}");
        assert!(entry.contains("<pubDate>"), "{entry}");
    }

    #[test]
    fn an_entry_links_the_url_alias_when_there_is_one() {
        let row = serde_json::json!({"id": "01234567-89ab-7def-8123-456789abcdef", "title": "T"});
        let aliases = HashMap::from([(
            "/item/01234567-89ab-7def-8123-456789abcdef".to_string(),
            "/blog/a-post".to_string(),
        )]);

        let entry = render_entry(&row, "https://example.com", &aliases);

        assert!(
            entry.contains("<link>https://example.com/blog/a-post</link>"),
            "{entry}"
        );
    }

    #[test]
    fn an_entry_without_a_description_omits_the_element() {
        let row = serde_json::json!({"id": "x", "title": "T"});

        let entry = render_entry(&row, "https://example.com", &HashMap::new());

        assert!(!entry.contains("<description>"), "{entry}");
    }

    #[test]
    fn a_description_in_the_field_value_shape_is_read() {
        let row = serde_json::json!({
            "id": "x",
            "title": "T",
            "field_body": {"value": "<p>Body text.</p>"},
        });

        let entry = render_entry(&row, "https://example.com", &HashMap::new());

        assert!(entry.contains("<![CDATA[<p>Body text.</p>]]>"), "{entry}");
    }

    #[test]
    fn xml_special_characters_in_a_title_are_escaped() {
        let row = serde_json::json!({"id": "x", "title": "Salt & <Pepper>"});

        let entry = render_entry(&row, "https://example.com", &HashMap::new());

        assert!(
            entry.contains("<title>Salt &amp; &lt;Pepper&gt;</title>"),
            "{entry}"
        );
    }

    /// A description containing `]]>` would otherwise close the CDATA section
    /// and inject markup into the feed.
    #[test]
    fn a_cdata_terminator_in_a_description_cannot_close_the_section() {
        let row = serde_json::json!({
            "id": "x",
            "title": "T",
            "summary": "before ]]><script>alert(1)</script> after",
        });

        let entry = render_entry(&row, "https://example.com", &HashMap::new());

        assert!(
            !entry.contains("]]><script>"),
            "the terminator must be split: {entry}"
        );
        assert!(entry.contains("]]]]><![CDATA[>"), "{entry}");
    }

    #[test]
    fn the_envelope_is_well_formed_rss_two() {
        let entries = vec![build_rss_item("One", "https://example.com/1", "", "")];

        let feed = build_rss_feed("My Feed", "https://example.com/rss.xml", "Desc", &entries);

        assert!(feed.starts_with("<?xml version=\"1.0\""));
        assert!(feed.contains("<rss version=\"2.0\""));
        assert!(feed.contains("<title>My Feed</title>"));
        assert!(feed.contains(
            "<atom:link href=\"https://example.com/rss.xml\" rel=\"self\" \
             type=\"application/rss+xml\"/>"
        ));
        assert!(feed.contains("<title>One</title>"));
        assert!(feed.ends_with("</channel>\n</rss>\n"));
    }
}
