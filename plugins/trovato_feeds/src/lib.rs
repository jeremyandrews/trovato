//! RSS/Atom feed generation plugin for Trovato.
//!
//! Provides RSS 2.0 feeds for site content. Registers menu routes for
//! feed URLs and includes helpers for building well-formed RSS XML.

use trovato_sdk::prelude::*;

/// Register the feeds admin permission.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        "administer feeds",
        "Configure RSS/Atom feed settings and manage feed endpoints",
    )]
}

/// Register feed routes.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/rss/insights.xml", "RSS: Insights Feed").callback("feed_insights"),
        MenuDefinition::new("/rss/planet-drupal.xml", "RSS: Planet Drupal Feed")
            .callback("feed_planet_drupal"),
    ]
}

/// Build an individual RSS 2.0 `<item>` element.
///
/// All text content is XML-escaped. Categories are rendered as separate
/// `<category>` elements. The `pub_date` must be in RFC 822 format
/// (e.g., `"Tue, 09 Apr 2026 12:00:00 +0000"`).
#[allow(clippy::unwrap_used)] // Infallible: write! to String cannot fail
#[allow(dead_code)] // called by plugin route callbacks at runtime
fn build_rss_item(
    title: &str,
    link: &str,
    description: &str,
    pub_date: &str,
    author: &str,
    categories: &[&str],
) -> String {
    use std::fmt::Write;

    let mut item = String::from("    <item>\n");
    write!(
        item,
        "      <title>{}</title>\n\
         \x20     <link>{}</link>\n\
         \x20     <description><![CDATA[{}]]></description>\n\
         \x20     <pubDate>{}</pubDate>\n\
         \x20     <author>{}</author>\n",
        xml_escape(title),
        xml_escape(link),
        description,
        xml_escape(pub_date),
        xml_escape(author),
    )
    .unwrap(); // Infallible: writing to String

    for cat in categories {
        writeln!(item, "      <category>{}</category>", xml_escape(cat)).unwrap(); // Infallible: writing to String
    }

    item.push_str("    </item>\n");
    item
}

/// Wrap RSS items in a complete RSS 2.0 XML envelope.
///
/// Produces a valid RSS 2.0 document with the given channel metadata
/// and pre-built item elements.
#[allow(clippy::unwrap_used)] // Infallible: write! to String cannot fail
#[allow(dead_code)] // called by plugin route callbacks at runtime
fn build_rss_feed(title: &str, link: &str, description: &str, items: &[String]) -> String {
    use std::fmt::Write;

    let mut feed = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\">\n\
         \x20 <channel>\n",
    );

    write!(
        feed,
        "    <title>{}</title>\n\
         \x20   <link>{}</link>\n\
         \x20   <description>{}</description>\n\
         \x20   <language>en</language>\n",
        xml_escape(title),
        xml_escape(link),
        xml_escape(description),
    )
    .unwrap(); // Infallible: writing to String

    for item in items {
        feed.push_str(item);
    }

    feed.push_str("  </channel>\n</rss>\n");
    feed
}

/// Escape special XML characters in text content.
#[allow(dead_code)] // called by build_rss_item and build_rss_feed
fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn perm_returns_feeds_permission() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0].name, "administer feeds");
    }

    #[test]
    fn menu_returns_two_feed_routes() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 2);

        let paths: Vec<&str> = menus.iter().map(|m| m.path.as_str()).collect();
        assert!(paths.contains(&"/rss/insights.xml"));
        assert!(paths.contains(&"/rss/planet-drupal.xml"));
    }

    #[test]
    fn build_rss_item_produces_valid_xml() {
        let item = build_rss_item(
            "Test Post",
            "https://example.com/blog/test",
            "A <b>test</b> post",
            "Tue, 09 Apr 2026 12:00:00 +0000",
            "author@example.com",
            &["Rust", "Web"],
        );
        assert!(item.contains("<title>Test Post</title>"));
        assert!(item.contains("<link>https://example.com/blog/test</link>"));
        assert!(item.contains("<![CDATA[A <b>test</b> post]]>"));
        assert!(item.contains("<category>Rust</category>"));
        assert!(item.contains("<category>Web</category>"));
        assert!(item.contains("<pubDate>Tue, 09 Apr 2026 12:00:00 +0000</pubDate>"));
    }

    #[test]
    fn build_rss_item_escapes_special_chars() {
        let item = build_rss_item(
            "Title & <More>",
            "https://example.com/a&b",
            "desc",
            "Mon, 01 Jan 2026 00:00:00 +0000",
            "a@b.com",
            &["A & B"],
        );
        assert!(item.contains("<title>Title &amp; &lt;More&gt;</title>"));
        assert!(item.contains("A &amp; B"));
    }

    #[test]
    fn build_rss_feed_wraps_items() {
        let item1 = build_rss_item(
            "Post 1",
            "https://example.com/1",
            "First",
            "Mon, 01 Jan 2026 00:00:00 +0000",
            "a@b.com",
            &[],
        );
        let item2 = build_rss_item(
            "Post 2",
            "https://example.com/2",
            "Second",
            "Tue, 02 Jan 2026 00:00:00 +0000",
            "a@b.com",
            &[],
        );
        let feed = build_rss_feed(
            "My Feed",
            "https://example.com",
            "A test feed",
            &[item1, item2],
        );

        assert!(feed.starts_with("<?xml version=\"1.0\""));
        assert!(feed.contains("<rss version=\"2.0\""));
        assert!(feed.contains("<title>My Feed</title>"));
        assert!(feed.contains("<title>Post 1</title>"));
        assert!(feed.contains("<title>Post 2</title>"));
        assert!(feed.contains("</channel>"));
        assert!(feed.ends_with("</rss>\n"));
    }

    #[test]
    fn build_rss_feed_empty_items() {
        let feed = build_rss_feed("Empty", "https://example.com", "No items", &[]);
        assert!(feed.contains("<title>Empty</title>"));
        assert!(!feed.contains("<item>"));
    }

    #[test]
    fn xml_escape_handles_all_special_chars() {
        assert_eq!(
            xml_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
    }
}
