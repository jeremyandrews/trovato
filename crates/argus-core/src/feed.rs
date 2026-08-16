//! RSS 2.0 and Atom 1.0 feed parsing (M1-5).
//!
//! Pure over a `&str` body — the streaming http host reassembles the bytes, and
//! this module turns them into [`ParsedArticle`]s. Uses `roxmltree` (a
//! read-only DOM parser with no network/filesystem/clock dependencies, so it
//! compiles cleanly to `wasm32-wasip1`).
//!
//! Both formats are handled by detecting the root element: `<rss>`/`<rdf:RDF>`
//! → RSS, `<feed>` → Atom. Date parsing is intentionally minimal (RFC-822 and
//! RFC-3339 to a unix timestamp); an unparseable or absent date yields
//! `published_at: None` rather than failing the whole feed.

use crate::error::{CoreError, CoreResult};
use crate::model::ParsedArticle;

/// Parse an RSS or Atom feed body into its articles.
///
/// # Errors
///
/// Returns [`CoreError::FeedParse`] if the body is not well-formed XML or is
/// neither an RSS nor an Atom document. Individual entries missing a link are
/// skipped (not fatal); a feed with zero usable entries parses to an empty Vec.
pub fn parse_feed(body: &str) -> CoreResult<Vec<ParsedArticle>> {
    let doc = roxmltree::Document::parse(body)
        .map_err(|e| CoreError::FeedParse(format!("malformed XML: {e}")))?;
    let root = doc.root_element();
    let name = root.tag_name().name();

    match name {
        "rss" | "RDF" => Ok(parse_rss(&doc)),
        "feed" => Ok(parse_atom(&doc)),
        other => Err(CoreError::FeedParse(format!(
            "unrecognized root element <{other}>; expected <rss>, <rdf:RDF>, or <feed>"
        ))),
    }
}

/// Local-name element-child lookup that ignores XML namespaces.
fn child_text<'a>(node: roxmltree::Node<'a, 'a>, local: &str) -> Option<String> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == local)
        .and_then(|c| c.text())
        .map(|t| t.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_rss(doc: &roxmltree::Document) -> Vec<ParsedArticle> {
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "item")
        .filter_map(|item| {
            let url = child_text(item, "link")?;
            let title = child_text(item, "title").unwrap_or_default();
            // RSS carries body text in <description> or <content:encoded>.
            let content = child_text(item, "encoded")
                .or_else(|| child_text(item, "description"))
                .unwrap_or_default();
            let published_at = child_text(item, "pubDate")
                .or_else(|| child_text(item, "date"))
                .and_then(|d| parse_date(&d));
            Some(ParsedArticle {
                url,
                title,
                content,
                published_at,
            })
        })
        .collect()
}

fn parse_atom(doc: &roxmltree::Document) -> Vec<ParsedArticle> {
    doc.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "entry")
        .filter_map(|entry| {
            let url = atom_link(entry)?;
            let title = child_text(entry, "title").unwrap_or_default();
            let content = child_text(entry, "content")
                .or_else(|| child_text(entry, "summary"))
                .unwrap_or_default();
            let published_at = child_text(entry, "published")
                .or_else(|| child_text(entry, "updated"))
                .and_then(|d| parse_date(&d));
            Some(ParsedArticle {
                url,
                title,
                content,
                published_at,
            })
        })
        .collect()
}

/// Extract the best link from an Atom `<entry>`.
///
/// Prefers `rel="alternate"` (or a link with no `rel`, which defaults to
/// alternate); falls back to the first link with an `href`.
fn atom_link(entry: roxmltree::Node) -> Option<String> {
    let links: Vec<roxmltree::Node> = entry
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() == "link")
        .collect();
    links
        .iter()
        .find(|l| matches!(l.attribute("rel"), Some("alternate") | None))
        .or_else(|| links.first())
        .and_then(|l| l.attribute("href"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Parse an RFC-3339 or RFC-822 date to a unix timestamp (seconds).
///
/// Deliberately dependency-free and lenient: recognizes the two shapes feeds
/// actually use and returns `None` on anything else. The pipeline treats a
/// missing publish date as acceptable, so precision here is not load-bearing.
fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    parse_rfc3339(s).or_else(|| parse_rfc822(s))
}

/// Days from the Unix epoch (1970-01-01) to `year-month-day` (proleptic Gregorian).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    // Howard Hinnant's civil-from-days algorithm, inverted.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) as i64 + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn to_timestamp(year: i64, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> i64 {
    days_from_civil(year, month, day) * 86_400 + hour as i64 * 3600 + min as i64 * 60 + sec as i64
}

/// Parse `2026-07-18T14:30:00Z` / `...+00:00` style timestamps.
fn parse_rfc3339(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    // Separator is 'T' or ' '.
    let hour: u32 = s.get(11..13)?.parse().ok()?;
    let min: u32 = s.get(14..16)?.parse().ok()?;
    let sec: u32 = s.get(17..19)?.parse().ok()?;
    if month == 0 || month > 12 || day == 0 || day > 31 || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    let base = to_timestamp(year, month, day, hour, min, sec);
    // Offset suffix: Z, or ±HH:MM.
    let rest = &s[19..];
    let offset = parse_tz_offset(rest);
    Some(base - offset)
}

fn parse_tz_offset(rest: &str) -> i64 {
    let rest = rest.trim();
    // Skip fractional seconds like ".123".
    let rest = rest.find(['+', '-', 'Z', 'z']).map_or("", |i| &rest[i..]);
    if rest.is_empty() || rest.starts_with('Z') || rest.starts_with('z') {
        return 0;
    }
    let sign = if rest.starts_with('-') { -1 } else { 1 };
    let digits: String = rest[1..].chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 2 {
        return 0;
    }
    let hh: i64 = digits.get(0..2).and_then(|d| d.parse().ok()).unwrap_or(0);
    let mm: i64 = digits.get(2..4).and_then(|d| d.parse().ok()).unwrap_or(0);
    sign * (hh * 3600 + mm * 60)
}

/// Parse `Sat, 18 Jul 2026 14:30:00 GMT` style (RFC-822/1123) dates.
fn parse_rfc822(s: &str) -> Option<i64> {
    // Drop an optional leading weekday token ending in a comma.
    let s = match s.split_once(", ") {
        Some((_, rest)) => rest,
        None => s,
    };
    let mut parts = s.split_whitespace();
    let day: u32 = parts.next()?.parse().ok()?;
    let month = month_from_abbr(parts.next()?)?;
    let year: i64 = {
        let y: i64 = parts.next()?.parse().ok()?;
        if y < 100 { 2000 + y } else { y }
    };
    let time = parts.next()?;
    let mut t = time.split(':');
    let hour: u32 = t.next()?.parse().ok()?;
    let min: u32 = t.next()?.parse().ok()?;
    let sec: u32 = t.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let tz = parts.next().unwrap_or("GMT");
    let offset = rfc822_offset(tz);
    if month == 0 || month > 12 || day == 0 || day > 31 {
        return None;
    }
    Some(to_timestamp(year, month, day, hour, min, sec) - offset)
}

fn month_from_abbr(m: &str) -> Option<u32> {
    Some(match m {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    })
}

fn rfc822_offset(tz: &str) -> i64 {
    match tz {
        "GMT" | "UTC" | "UT" | "Z" => 0,
        "EST" => -5 * 3600,
        "EDT" => -4 * 3600,
        "CST" => -6 * 3600,
        "CDT" => -5 * 3600,
        "MST" => -7 * 3600,
        "MDT" => -6 * 3600,
        "PST" => -8 * 3600,
        "PDT" => -7 * 3600,
        _ => parse_tz_offset(tz),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0"?>
    <rss version="2.0">
      <channel>
        <title>Example</title>
        <item>
          <title>First Post</title>
          <link>https://example.com/first</link>
          <description>Body one</description>
          <pubDate>Sat, 18 Jul 2026 14:30:00 GMT</pubDate>
        </item>
        <item>
          <title>Second Post</title>
          <link>https://example.com/second</link>
          <description>Body two</description>
        </item>
      </channel>
    </rss>"#;

    const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
    <feed xmlns="http://www.w3.org/2005/Atom">
      <title>Example</title>
      <entry>
        <title>Atom One</title>
        <link rel="alternate" href="https://example.com/atom-one"/>
        <summary>Summary one</summary>
        <published>2026-07-18T14:30:00Z</published>
      </entry>
      <entry>
        <title>Atom Two</title>
        <link href="https://example.com/atom-two"/>
        <content>Content two</content>
        <updated>2026-07-17T09:00:00+02:00</updated>
      </entry>
    </feed>"#;

    #[test]
    fn parses_rss() {
        let arts = parse_feed(RSS).unwrap();
        assert_eq!(arts.len(), 2);
        assert_eq!(arts[0].url, "https://example.com/first");
        assert_eq!(arts[0].title, "First Post");
        assert_eq!(arts[0].content, "Body one");
        assert!(arts[0].published_at.is_some());
        assert!(arts[1].published_at.is_none());
    }

    #[test]
    fn parses_atom() {
        let arts = parse_feed(ATOM).unwrap();
        assert_eq!(arts.len(), 2);
        assert_eq!(arts[0].url, "https://example.com/atom-one");
        assert_eq!(arts[0].content, "Summary one");
        assert_eq!(arts[1].url, "https://example.com/atom-two");
        assert_eq!(arts[1].content, "Content two");
    }

    #[test]
    fn rfc3339_utc_epoch() {
        // 2026-07-18T14:30:00Z
        let ts = parse_date("2026-07-18T14:30:00Z").unwrap();
        assert_eq!(ts, to_timestamp(2026, 7, 18, 14, 30, 0));
    }

    #[test]
    fn rfc3339_offset_applied() {
        // 09:00:00+02:00 == 07:00:00Z
        let ts = parse_date("2026-07-17T09:00:00+02:00").unwrap();
        assert_eq!(ts, to_timestamp(2026, 7, 17, 7, 0, 0));
    }

    #[test]
    fn rfc822_gmt() {
        let ts = parse_date("Sat, 18 Jul 2026 14:30:00 GMT").unwrap();
        assert_eq!(ts, to_timestamp(2026, 7, 18, 14, 30, 0));
    }

    #[test]
    fn epoch_zero_sanity() {
        assert_eq!(to_timestamp(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(to_timestamp(2000, 1, 1, 0, 0, 0), 946_684_800);
    }

    #[test]
    fn rejects_non_feed() {
        assert!(parse_feed("<html><body>nope</body></html>").is_err());
        assert!(parse_feed("not xml at all <<<").is_err());
    }

    #[test]
    fn skips_entries_without_link() {
        let rss = r#"<rss version="2.0"><channel>
          <item><title>No link</title></item>
          <item><title>Has link</title><link>https://x.test/a</link></item>
        </channel></rss>"#;
        let arts = parse_feed(rss).unwrap();
        assert_eq!(arts.len(), 1);
        assert_eq!(arts[0].url, "https://x.test/a");
    }
}
