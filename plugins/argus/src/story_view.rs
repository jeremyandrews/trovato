//! The story page fragment: what `tap_item_view` appends to an `argus_story`
//! Item's page (M3).
//!
//! # Why a plugin renders its own HTML here
//!
//! The kernel's item page renders declared fields generically — for a field
//! stored as `{"value": …}` that means printing the JSON — and a plugin cannot
//! ship a template: the theme engine loads from the site's `templates/`
//! directory and has no plugin path. `tap_item_view`'s return value is appended
//! to the page's children (`crates/kernel/src/routes/item.rs`, "Include plugin
//! render outputs"), so building the fragment here is the surface the frozen
//! contract offers.
//!
//! # Why the markup uses single-quoted attributes
//!
//! A tap's return value is JSON-serialized by the `#[plugin_tap]` macro
//! (`crates/plugin-sdk-macros/src/lib.rs`, `serde_json::to_string(&result)`), and
//! the item route appends that serialized form to the page **without decoding
//! it** (`crates/kernel/src/routes/item.rs`, "Include plugin render outputs").
//! So a `String`-returning view tap reaches the page as a JSON string literal:
//! wrapped in quotes, with every inner `"` turned into `\"`. Double-quoted
//! attributes would arrive as `class=\` followed by a stray `argus-story\`,
//! which mangles the markup outright.
//!
//! Single-quoted attributes, and an [`escape`] that emits `&quot;`/`&#x27;`
//! rather than raw quotes, mean this fragment contains **no character serde
//! escapes** — so the only damage the round trip does is the pair of quotes
//! wrapping the whole fragment. That is as close to correct as the frozen
//! contract allows; the defect itself is `G-VIEW-OUTPUT-JSON-ENCODED`, and
//! `plugins/trovato_series` has it too.
//!
//! # Escaping is this module's job
//!
//! That append is **verbatim** — the kernel neither escapes nor sanitizes a
//! plugin's view output, and the SDK ships no escaping helper. Every value that
//! reaches the output below therefore goes through [`escape`], including the
//! synthesized summary, source names and entity names, all of which originate in
//! model output over fetched third-party content. Recorded as
//! `G-VIEW-HTML-UNESCAPED`.
//!
//! The rendering is a pure function of the Item JSON, so it is unit-tested
//! natively with no host in sight.

use argus_core::reader::Reaction;
use serde_json::Value;

/// Number of credited sources rendered before the list is cut.
const MAX_SOURCES: usize = 25;

/// Number of entities rendered before the list is cut.
const MAX_ENTITIES: usize = 12;

/// Escape text for interpolation into HTML.
///
/// Covers the five characters that matter in element content and in an
/// attribute value. There is no SDK helper for this and the kernel does not
/// escape plugin view output, so this is the only thing standing between
/// model-summarized third-party content and the page.
///
/// It also removes the two things HTML does not care about but **JSON does**:
/// a literal backslash becomes `&#x5C;` and any control character becomes a
/// space. Neither is an HTML concern; both are here because the kernel appends
/// this fragment's JSON-serialized form to the page undecoded
/// (`G-VIEW-OUTPUT-JSON-ENCODED`), so a `\` or a newline in the text would reach
/// a reader as a literal `\\` or `\n`. Newlines are whitespace in HTML anyway,
/// so collapsing them costs nothing; paragraph breaks are handled before the
/// text gets here.
pub fn escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            '\\' => out.push_str("&#x5C;"),
            c if c.is_control() => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

/// Read a field that may be stored flat or wrapped as `{"value": …}`.
///
/// Both shapes are live in one install: M2 writes story fields wrapped, the
/// admin content form writes flat (`G-ITEM-FORM-MISMATCH`). Accepting either is
/// cheaper and less brittle than depending on which writer touched the Item
/// last.
fn field(item: &Value, name: &str) -> Option<Value> {
    let raw = item.get("fields")?.get(name)?;
    match raw.get("value") {
        Some(inner) => Some(inner.clone()),
        None => Some(raw.clone()),
    }
}

/// Read a field as a string, whatever JSON scalar it is stored as.
fn field_str(item: &Value, name: &str) -> String {
    match field(item, name) {
        Some(Value::String(s)) => s,
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// One credited source, as the `field_sources` json carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Source {
    /// Outlet or feed name.
    name: String,
    /// Article headline.
    title: String,
    /// `true` when this outlet re-ran a report already credited.
    duplicate: bool,
}

/// Parse `field_sources` — a JSON array serialized into a text field.
///
/// Tolerant by construction: a story whose sources json is missing, empty, or
/// unparseable still renders its summary. A page that 500s because one field is
/// malformed would be a worse answer than a page with no source list.
fn parse_sources(item: &Value) -> Vec<Source> {
    let raw = field_str(item, "field_sources");
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    parsed
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| {
                    let name = e.get("source").and_then(Value::as_str).unwrap_or("").trim();
                    let title = e.get("title").and_then(Value::as_str).unwrap_or("").trim();
                    if name.is_empty() && title.is_empty() {
                        return None;
                    }
                    Some(Source {
                        name: name.to_string(),
                        title: title.to_string(),
                        duplicate: e.get("contribution").and_then(Value::as_str)
                            == Some("duplicate"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse `field_entities`, which the M2 sync writes as a JSON array of names.
///
/// Accepts a bare comma-separated string too, so a story written before the
/// field was json-shaped still renders something rather than nothing.
fn parse_entities(item: &Value) -> Vec<String> {
    let raw = field_str(item, "field_entities");
    if raw.trim().is_empty() {
        return Vec::new();
    }
    if let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(&raw) {
        return entries
            .iter()
            .filter_map(|e| {
                e.as_str()
                    .or_else(|| e.get("canonical_name").and_then(Value::as_str))
            })
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Render the story fragment for an `argus_story` Item.
///
/// `reactions` is what this reader currently holds on this story; it is empty
/// for an anonymous viewer, and always empty of anything a reader put there
/// themselves in 1.0, since nothing can write a reaction yet
/// (`M3-DESIGN.md` Decision 5).
///
/// Returns an empty string for anything that is not a story, so the tap can be
/// called for every Item type without a guard at the call site.
pub fn render(item: &Value, reactions: &[Reaction]) -> String {
    if item.get("type").and_then(Value::as_str) != Some("argus_story") {
        return String::new();
    }

    let mut html = String::from(r#"<div class='argus-story'>"#);

    let summary = field_str(item, "field_summary");
    if !summary.trim().is_empty() {
        html.push_str(r#"<div class='argus-story__summary'>"#);
        // The synthesis is prose in paragraphs; render the breaks rather than
        // collapsing several paragraphs into one wall of text.
        for para in summary.split("\n\n").filter(|p| !p.trim().is_empty()) {
            html.push_str("<p>");
            html.push_str(&escape(para.trim()));
            html.push_str("</p>");
        }
        html.push_str("</div>");
    }

    let entities = parse_entities(item);
    if !entities.is_empty() {
        html.push_str(r#"<ul class='argus-story__entities'>"#);
        for name in entities.iter().take(MAX_ENTITIES) {
            html.push_str(r#"<li class='argus-story__entity'>"#);
            html.push_str(&escape(name));
            html.push_str("</li>");
        }
        html.push_str("</ul>");
    }

    let sources = parse_sources(item);
    if !sources.is_empty() {
        html.push_str(r#"<section class='argus-story__sources'><h2>Sources</h2><ul>"#);
        for source in sources.iter().take(MAX_SOURCES) {
            html.push_str(if source.duplicate {
                r#"<li class='argus-story__source argus-story__source--duplicate'>"#
            } else {
                r#"<li class='argus-story__source'>"#
            });
            html.push_str(r#"<span class='argus-story__source-name'>"#);
            html.push_str(&escape(&source.name));
            html.push_str("</span>");
            if !source.title.is_empty() {
                html.push_str(r#"<span class='argus-story__source-title'>"#);
                html.push_str(&escape(&source.title));
                html.push_str("</span>");
            }
            html.push_str("</li>");
        }
        if sources.len() > MAX_SOURCES {
            // Say what was cut rather than letting the list end silently at 25.
            html.push_str(r#"<li class='argus-story__source-more'>"#);
            html.push_str(&escape(&format!(
                "and {} more",
                sources.len() - MAX_SOURCES
            )));
            html.push_str("</li>");
        }
        html.push_str("</ul></section>");
    }

    if !reactions.is_empty() {
        html.push_str(r#"<ul class='argus-story__reactions'>"#);
        for reaction in reactions {
            html.push_str(&format!(
                r#"<li class='argus-story__reaction argus-story__reaction--{0}'>{0}</li>"#,
                escape(reaction.as_str())
            ));
        }
        html.push_str("</ul>");
    }

    // The kernel serves comments at /api/item/{id}/comments but renders none on
    // the item page (`templates/elements/comments.html` is orphaned —
    // G-COMMENTS-UNRENDERED), so the fragment carries the mount point and the
    // item id a client needs to fetch and post them.
    if let Some(id) = item.get("id").and_then(Value::as_str) {
        html.push_str(&format!(
            r#"<section class='argus-story__comments' data-comments-for='{}'></section>"#,
            escape(id)
        ));
    }

    html.push_str("</div>");
    html
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn story(fields: Value) -> Value {
        json!({
            "id": "019400aa-0000-7000-8000-00000000abcd",
            "type": "argus_story",
            "title": "A story",
            "fields": fields,
        })
    }

    #[test]
    fn renders_nothing_for_another_content_type() {
        let item = json!({ "id": "x", "type": "blog", "fields": {} });
        assert!(render(&item, &[]).is_empty());
    }

    #[test]
    fn renders_the_summary_as_paragraphs() {
        let html = render(
            &story(json!({
                "field_summary": { "value": "First para.\n\nSecond para." }
            })),
            &[],
        );
        assert!(html.contains("<p>First para.</p>"));
        assert!(html.contains("<p>Second para.</p>"));
    }

    #[test]
    fn reads_a_flat_field_as_well_as_a_wrapped_one() {
        // M2 writes {"value": ..}; the admin content form writes flat.
        let html = render(&story(json!({ "field_summary": "Flat prose." })), &[]);
        assert!(html.contains("<p>Flat prose.</p>"));
    }

    #[test]
    fn escapes_a_summary_that_carries_markup() {
        // The summary is model output over third-party content and the kernel
        // appends this fragment verbatim, so this is the load-bearing test.
        let html = render(
            &story(json!({
                "field_summary": { "value": "<script>alert('xss')</script>" }
            })),
            &[],
        );
        assert!(!html.contains("<script>"), "html was {html}");
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn escapes_a_source_name_that_carries_markup() {
        let sources = json!([
            { "source": "<img src=x onerror=alert(1)>", "title": "Report", "contribution": "member" }
        ])
        .to_string();
        let html = render(
            &story(json!({ "field_sources": { "value": sources } })),
            &[],
        );
        assert!(!html.contains("<img"), "html was {html}");
        assert!(html.contains("&lt;img"));
    }

    #[test]
    fn escapes_an_entity_name_that_carries_markup() {
        let entities = json!(["<b>Acme</b>"]).to_string();
        let html = render(
            &story(json!({ "field_entities": { "value": entities } })),
            &[],
        );
        assert!(!html.contains("<b>Acme"), "html was {html}");
        assert!(html.contains("&lt;b&gt;Acme"));
    }

    #[test]
    fn credits_each_source_and_marks_duplicates() {
        let sources = json!([
            { "source": "Ars Technica", "title": "Original report", "contribution": "member" },
            { "source": "The Verge", "title": "Same report", "contribution": "duplicate" },
        ])
        .to_string();
        let html = render(
            &story(json!({ "field_sources": { "value": sources } })),
            &[],
        );
        assert!(html.contains("Ars Technica"));
        assert!(html.contains("The Verge"));
        assert!(html.contains("argus-story__source--duplicate"));
    }

    #[test]
    fn an_unparseable_sources_field_still_renders_the_summary() {
        let html = render(
            &story(json!({
                "field_summary": { "value": "The prose survives." },
                "field_sources": { "value": "{not json" },
            })),
            &[],
        );
        assert!(html.contains("The prose survives."));
        assert!(!html.contains("argus-story__sources"));
    }

    #[test]
    fn a_long_source_list_is_cut_and_says_so() {
        let entries: Vec<Value> = (0..MAX_SOURCES + 3)
            .map(|i| json!({ "source": format!("Outlet {i}"), "title": "t" }))
            .collect();
        let html = render(
            &story(json!({
                "field_sources": { "value": Value::Array(entries).to_string() }
            })),
            &[],
        );
        assert!(html.contains("and 3 more"), "html was {html}");
        assert!(!html.contains("Outlet 26"));
    }

    #[test]
    fn entities_accept_a_bare_comma_separated_string() {
        let html = render(
            &story(json!({
                "field_entities": { "value": "Acme Corp, Jane Roe" }
            })),
            &[],
        );
        assert!(html.contains("Acme Corp"));
        assert!(html.contains("Jane Roe"));
    }

    #[test]
    fn the_comment_mount_carries_the_item_id() {
        let html = render(&story(json!({})), &[]);
        assert!(html.contains("data-comments-for='019400aa-0000-7000-8000-00000000abcd'"));
    }

    #[test]
    fn an_empty_story_still_renders_a_container() {
        let html = render(&story(json!({})), &[]);
        assert!(html.starts_with(r#"<div class='argus-story'>"#));
        assert!(html.ends_with("</div>"));
    }

    #[test]
    fn the_fragment_contains_nothing_json_serialization_would_escape() {
        // The item route appends this fragment's *serialized* form to the page
        // without decoding it (G-VIEW-OUTPUT-JSON-ENCODED). Keeping `"` and `\`
        // out of the output is what stops that round trip from mangling the
        // markup, so it is asserted rather than left to convention.
        let sources = json!([
            { "source": "O'Brien \"News\"", "title": "A \\ backslash", "contribution": "member" }
        ])
        .to_string();
        let html = render(
            &story(json!({
                "field_summary": { "value": "Quotes \" and a backslash \\ and an apostrophe '." },
                "field_sources": { "value": sources },
                "field_entities": { "value": json!(["Acme \"Corp\""]).to_string() },
            })),
            &[Reaction::Bookmark],
        );
        assert!(!html.contains('"'), "raw double quote in: {html}");
        assert!(!html.contains('\\'), "raw backslash in: {html}");
        let serialized = serde_json::to_string(&html).expect("serializes");
        assert_eq!(
            serialized,
            format!("\"{html}\""),
            "serialization must add only the wrapping quotes"
        );
    }

    #[test]
    fn escape_covers_the_attribute_breaking_characters() {
        assert_eq!(
            escape(r#"<a href="x">&'"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#x27;"
        );
    }
}
