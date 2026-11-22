//! The search snippet: built once, escaped once.
//!
//! The rule, in order:
//!
//! 1. **Strip the source.** The search query strips HTML tags from the stored
//!    title and body *before* highlighting ([`SOURCE_SQL`]), and removes the
//!    two marker characters from it, so stored content can neither show up as
//!    markup nor forge a highlight.
//! 2. **Highlight with fixed tokens.** `ts_headline` wraps each match in
//!    [`START`] and [`STOP`] ([`HEADLINE_OPTIONS`]), two private-use
//!    characters, not in `<mark>`.
//! 3. **Escape once.** [`render`] splits the headline at the markers, decodes
//!    the HTML character references the stripped source still holds (a stored
//!    `&amp;` is the text `&`) with the kernel's one text extractor,
//!    [`text_of`], and HTML-escapes each piece exactly once.
//! 4. **Markers become `<mark>`.** Only then, and only from the markers.
//!
//! The result is the only HTML in a [`super::SearchResult`] snippet, which is
//! why `templates/search.html` may print `result.snippet | safe` and why no
//! route escapes it again.

use crate::content::page_meta::text_of;
use crate::routes::helpers::html_escape;

/// The highlight start marker `ts_headline` inserts (a private-use character).
pub(crate) const START: char = '\u{E000}';
/// The highlight stop marker `ts_headline` inserts (a private-use character).
pub(crate) const STOP: char = '\u{E001}';

/// The `ts_headline` options: the fixed markers, and the snippet length.
pub(crate) const HEADLINE_OPTIONS: &str =
    "StartSel=\"\u{E000}\", StopSel=\"\u{E001}\", MaxWords=35, MinWords=15";

/// The headline source, as a SQL expression over `item`: title and long text
/// body, every tag replaced by a space, the two markers removed.
///
/// A tag is `<` followed by a letter, `/`, `!` or `?`, up to the next `>`, the
/// way an HTML tokenizer opens one, so a plain-text `3 < 4` is left alone. The
/// long text field goes by two names (BL-21): `field_body` where a content type
/// declared it, `body` on the kernel's own `page`; both, in that order, or a
/// `page` matched only on its title gets a snippet with no body text in it.
pub(crate) const SOURCE_SQL: &str = r#"translate(
        regexp_replace(
            COALESCE(title, '') || ' ' || COALESCE(
                fields->'field_body'->>'value',
                fields->>'field_body',
                fields->'body'->>'value',
                fields->>'body',
                ''
            ),
            '<[A-Za-z/!?][^>]*>', ' ', 'g'
        ),
        U&'\E000\E001', ''
    )"#;

/// Turn a `ts_headline` result into the snippet's HTML: text escaped once, the
/// marked spans wrapped in `<mark>`.
///
/// Only the markers produce `<mark>`; anything else in the input, a `<mark>`
/// or `<script>` that reached the headline included, comes out as text. A marker left open at
/// the end is closed.
pub fn render(headline: &str) -> String {
    let mut out = String::with_capacity(headline.len() + 16);
    let mut text = String::new();
    let mut open = false;
    for c in headline.chars() {
        if c != START && c != STOP {
            text.push(c);
            continue;
        }
        out.push_str(&html_escape(&text_of(&text)));
        text.clear();
        if c == START && !open {
            out.push_str("<mark>");
            open = true;
        } else if c == STOP && open {
            out.push_str("</mark>");
            open = false;
        }
    }
    out.push_str(&html_escape(&text_of(&text)));
    if open {
        out.push_str("</mark>");
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn hl(s: &str) -> String {
        s.replace("[[", &START.to_string())
            .replace("]]", &STOP.to_string())
    }

    #[test]
    fn markers_become_mark_and_text_is_escaped_once() {
        assert_eq!(
            render(&hl("Tom &amp; Jerry said \"[[cheese]]\"")),
            "Tom &amp; Jerry said &quot;<mark>cheese</mark>&quot;"
        );
    }

    #[test]
    fn stored_markup_never_survives() {
        // The query strips tags before highlighting; a tag that reached the
        // headline anyway is dropped here, never passed through.
        assert_eq!(
            render(&hl("<mark>x</mark> <script>alert(1)</script> [[y]]")),
            "x  <mark>y</mark>"
        );
        assert_eq!(
            render(&hl("<img src=x onerror=alert(1)>[[y]]")),
            "<mark>y</mark>"
        );
    }

    #[test]
    fn escaped_markup_in_the_source_shows_as_that_text() {
        assert_eq!(
            render("&lt;mark&gt;planted&lt;/mark&gt;"),
            "&lt;mark&gt;planted&lt;/mark&gt;"
        );
    }

    #[test]
    fn plain_text_is_unchanged_but_for_escaping() {
        assert_eq!(render("3 < 4 & R&D"), "3 &lt; 4 &amp; R&amp;D");
    }

    #[test]
    fn several_marks_and_unbalanced_markers() {
        assert_eq!(
            render(&hl("[[a]] b [[c]]")),
            "<mark>a</mark> b <mark>c</mark>"
        );
        assert_eq!(render(&hl("]]a [[b")), "a <mark>b</mark>");
    }

    #[test]
    fn references_decode_once() {
        assert_eq!(
            render("it&#39;s &hellip; &copy; &amp;lt;"),
            "it&#x27;s … © &amp;lt;"
        );
    }
}
