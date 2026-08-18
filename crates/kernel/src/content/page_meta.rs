//! Head metadata for a rendered page: meta description, canonical link, and
//! the Open Graph / Twitter card tags.
//!
//! This lives in the kernel because `<head>` is not reachable from a plugin. A
//! plugin implementing `tap_item_view` returns markup that the item route
//! appends to the item's body, so the best it can do is emit body-level
//! metadata; `tap_item_view_alter`, which could rewrite the surrounding
//! document, is declared in the WIT contract but never dispatched. Without a
//! kernel-side path an item can carry a description and a canonical URL that no
//! crawler and no link unfurler ever sees.
//!
//! What this module does not do: decide policy. It derives a default from the
//! item's own fields, and a theme is free to override any of it in its own
//! `head` block. Per-item SEO overrides (an explicit meta title, a `noindex`
//! robots directive) belong to whichever plugin owns those fields; this is the
//! floor, not the ceiling.

use serde::Serialize;
use trovato_sdk::types::{FieldDefinition, FieldType};

use crate::models::Item;

/// Maximum length of a derived meta description, in bytes of UTF-8.
///
/// 160 is the width search results have truncated at for years. It is a
/// convention rather than a specification, and it is applied only to
/// descriptions this module derives from body text: a description supplied as
/// a field is the author's to size.
const DESCRIPTION_MAX_LEN: usize = 160;

/// The head metadata a page template needs.
///
/// Every field is optional because every field is derived: an item with no
/// body text has no description to emit, and emitting an empty
/// `<meta name="description">` is worse than emitting none. Templates guard
/// each one individually.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PageMeta {
    /// Plain-text summary for `<meta name="description">`, HTML stripped.
    pub description: Option<String>,

    /// Absolute URL of the page's canonical location.
    pub canonical: Option<String>,

    /// `og:title`. The item title, separate from the `<title>` element so a
    /// theme can suffix the site name into one without affecting the other.
    pub og_title: Option<String>,

    /// `og:type`: `article` for the item types that are articles, else
    /// `website`.
    pub og_type: Option<String>,

    /// `og:url`. The same absolute URL as [`Self::canonical`].
    pub og_url: Option<String>,

    /// `og:site_name`.
    pub og_site_name: Option<String>,

    /// `og:image`, absolute. Taken from the item's first image block, which is
    /// the only image the kernel can identify without a theme telling it which
    /// field is the lead image.
    pub og_image: Option<String>,

    /// `article:published_time`, RFC 3339. Only set when [`Self::og_type`] is
    /// `article`.
    pub published_time: Option<String>,

    /// `article:modified_time`, RFC 3339. Only set when [`Self::og_type`] is
    /// `article`.
    pub modified_time: Option<String>,
}

impl PageMeta {
    /// Derive the head metadata for an item page.
    ///
    /// `path` is the item's canonical path on this site, alias included when it
    /// has one; `site_url` is the site's public base URL (`SITE_URL`), used to
    /// make `og:url` and `og:image` absolute as Open Graph requires. `fields`
    /// is the item's content type field definitions, needed to tell a Blocks
    /// field from a JSON field that merely holds an array.
    pub fn for_item(
        item: &Item,
        path: &str,
        site_url: &str,
        site_name: &str,
        fields: &[FieldDefinition],
    ) -> Self {
        let url = resolve_url(site_url, path);
        let is_article = is_article_type(&item.item_type);

        let (published_time, modified_time) = if is_article {
            (rfc3339(item.created), rfc3339(item.changed))
        } else {
            (None, None)
        };

        Self {
            description: derive_description(item, fields),
            canonical: url.clone(),
            og_title: non_empty(item.title.trim()),
            og_type: Some(if is_article { "article" } else { "website" }.to_string()),
            og_url: url,
            og_site_name: non_empty(site_name.trim()),
            og_image: first_block_image(item, fields).and_then(|src| resolve_url(site_url, &src)),
            published_time,
            modified_time,
        }
    }
}

/// Whether an item type's pages are articles for Open Graph purposes.
///
/// Mirrors the mapping `trovato_seo` uses for its JSON-LD `@type`, so the
/// structured data and the Open Graph tags on one page cannot disagree about
/// what kind of thing the page is.
fn is_article_type(item_type: &str) -> bool {
    matches!(item_type, "blog" | "article" | "news")
}

/// Derive a plain-text description from the item's body text.
///
/// Order: `field_description`, then `field_body`, then the first paragraph of
/// the first Blocks field. The two field names are the pair `trovato_seo`
/// already reads, and the block fallback covers block-editor content types,
/// which have no `field_body` at all.
fn derive_description(item: &Item, fields: &[FieldDefinition]) -> Option<String> {
    let from_field =
        text_field(item, "field_description").or_else(|| text_field(item, "field_body"));

    let raw = match from_field {
        Some(text) => text,
        None => first_block_paragraph(item, fields)?,
    };

    let plain = plain_text(&raw);
    non_empty(truncate_on_word(&plain, DESCRIPTION_MAX_LEN).as_str())
}

/// Read a text field's value.
///
/// Field values are either `{"value": "..."}` or a bare string, the same two
/// shapes the embedding text builder handles.
fn text_field(item: &Item, name: &str) -> Option<String> {
    let value = item.fields.get(name)?;
    let text = value
        .get("value")
        .and_then(|v| v.as_str())
        .or_else(|| value.as_str())?;
    non_empty(text)
}

/// The item's Blocks fields, in content-type declaration order.
fn blocks_fields<'a>(
    item: &'a Item,
    fields: &'a [FieldDefinition],
) -> impl Iterator<Item = &'a Vec<serde_json::Value>> {
    fields
        .iter()
        .filter(|f| matches!(f.field_type, FieldType::Blocks))
        .filter_map(|f| item.fields.get(&f.field_name))
        .filter_map(|v| v.as_array())
}

/// Text of the first paragraph block in the item's first Blocks field.
fn first_block_paragraph(item: &Item, fields: &[FieldDefinition]) -> Option<String> {
    blocks_fields(item, fields)
        .flatten()
        .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("paragraph"))
        .filter_map(|block| {
            block
                .get("data")
                .and_then(|d| d.get("text"))
                .and_then(|t| t.as_str())
        })
        .find_map(non_empty)
}

/// Source URL of the first image block in the item's Blocks fields.
///
/// Reads both the current block shape (`{"file": {"url": ...}}`) and the
/// legacy flat one (`{"url": ...}`), matching the block renderer.
fn first_block_image(item: &Item, fields: &[FieldDefinition]) -> Option<String> {
    blocks_fields(item, fields)
        .flatten()
        .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("image"))
        .filter_map(|block| {
            let data = block.get("data")?;
            data.get("file")
                .and_then(|f| f.get("url"))
                .and_then(|v| v.as_str())
                .or_else(|| data.get("url").and_then(|v| v.as_str()))
        })
        .find_map(non_empty)
}

/// Resolve a path against the site's base URL, encoded for an HTML attribute
/// the template emits with `| safe`.
///
/// Two steps, and both are load-bearing.
///
/// `Url::join` does the resolving with URL semantics rather than string
/// concatenation: a relative path resolves against the base, an absolute URL is
/// returned as it is (so a CDN-hosted image is not rewritten into this site),
/// and a protocol-relative one picks up the base's scheme. It also percent
/// encodes, which is what makes the result safe to emit unescaped: after it,
/// the serialization cannot contain `"`, `<` or `>`, so nothing here can close
/// an attribute or open a tag.
///
/// Then `&` becomes `&amp;`, the one character left that an HTML parser would
/// read as the start of an entity. The alternative — leaving the value to
/// Tera's escaper — is safe but emits `&#x2F;` for every slash in the URL,
/// which is legal HTML that crawlers and link unfurlers doing their own naive
/// parsing get wrong.
///
/// `None` when there is nothing to resolve, or when the result is not an
/// http(s) URL, which is the case for a `javascript:` or `data:` value reaching
/// here from a field. A `site_url` that does not parse also yields `None`; that
/// configuration does not reach a running site, since the WebAuthn relying
/// party parses the same value at startup and refuses to build.
fn resolve_url(site_url: &str, path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }

    let base = url::Url::parse(site_url.trim()).ok()?;
    let resolved = base.join(path).ok()?;
    if !matches!(resolved.scheme(), "http" | "https") {
        return None;
    }

    Some(resolved.to_string().replace('&', "&amp;"))
}

/// Format a Unix timestamp as RFC 3339, as `article:published_time` wants it.
fn rfc3339(timestamp: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(timestamp, 0).map(|dt| dt.to_rfc3339())
}

/// Reduce markup to the plain text a meta description carries: tags stripped,
/// entities decoded, whitespace collapsed to single spaces.
fn plain_text(raw: &str) -> String {
    let stripped = crate::content::item_service::strip_html(raw);
    decode_basic_entities(&stripped)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Decode the entities an HTML serializer emits in a text node.
///
/// Stripping tags with ammonia round-trips through a serializer, so `&` in the
/// input comes back as `&amp;`. Tera escapes the value again when it renders
/// the attribute, so leaving it encoded here would put a literal `&amp;amp;` in
/// the description. `&amp;` is decoded last: decoding it first would turn the
/// encoded text `&amp;lt;` into a real `<`.
fn decode_basic_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// Truncate to at most `max_len` bytes, breaking at a word boundary and
/// marking the cut with an ellipsis.
fn truncate_on_word(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }

    // Leave room for the ellipsis, then walk back to a char boundary.
    let mut end = max_len.saturating_sub(3);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let head = &s[..end];

    // Prefer the last word boundary, unless that throws away most of the text.
    let cut = match head.rfind(char::is_whitespace) {
        Some(space) if space > end / 2 => space,
        _ => end,
    };

    format!("{}...", head[..cut].trim_end())
}

/// `Some(trimmed)` for a string with content, `None` for one without.
fn non_empty(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn field(name: &str, field_type: FieldType) -> FieldDefinition {
        FieldDefinition {
            field_name: name.to_string(),
            field_type,
            label: name.to_string(),
            required: false,
            cardinality: 1,
            settings: serde_json::json!({}),
            personal_data: false,
        }
    }

    fn item(item_type: &str, fields: serde_json::Value) -> Item {
        Item {
            id: Uuid::nil(),
            current_revision_id: None,
            item_type: item_type.to_string(),
            title: "A Title".to_string(),
            author_id: Uuid::nil(),
            status: 1,
            created: 1_700_000_000,
            changed: 1_700_000_500,
            promote: 0,
            sticky: 0,
            fields,
            stage_id: Uuid::nil(),
            language: "en".to_string(),
            item_group_id: Uuid::nil(),
            retention_days: None,
        }
    }

    #[test]
    fn description_prefers_the_description_field_over_the_body() {
        let item = item(
            "page",
            serde_json::json!({
                "field_description": {"value": "The summary."},
                "field_body": {"value": "The body."},
            }),
        );

        let meta = PageMeta::for_item(&item, "/item/x", "https://example.com", "Site", &[]);

        assert_eq!(meta.description.as_deref(), Some("The summary."));
    }

    #[test]
    fn description_falls_back_to_the_body_field() {
        let item = item(
            "page",
            serde_json::json!({"field_body": {"value": "The body."}}),
        );

        let meta = PageMeta::for_item(&item, "/item/x", "https://example.com", "Site", &[]);

        assert_eq!(meta.description.as_deref(), Some("The body."));
    }

    #[test]
    fn description_reads_a_bare_string_field_value() {
        let item = item("page", serde_json::json!({"field_body": "Bare string."}));

        let meta = PageMeta::for_item(&item, "/item/x", "https://example.com", "Site", &[]);

        assert_eq!(meta.description.as_deref(), Some("Bare string."));
    }

    #[test]
    fn description_strips_markup_and_collapses_whitespace() {
        let item = item(
            "page",
            serde_json::json!({
                "field_body": {"value": "<p>First   line</p>\n<p>Second <em>line</em></p>"},
            }),
        );

        let meta = PageMeta::for_item(&item, "/item/x", "https://example.com", "Site", &[]);

        assert_eq!(
            meta.description.as_deref(),
            Some("First line Second line"),
            "tags must go and runs of whitespace must collapse"
        );
    }

    /// The reason this module decodes entities at all: ammonia re-encodes `&`
    /// on its way out, and Tera encodes it again when it renders the attribute.
    /// Leaving it encoded here ships `&amp;amp;` to the crawler.
    #[test]
    fn description_decodes_entities_so_the_template_escapes_once() {
        let item = item(
            "page",
            serde_json::json!({"field_body": {"value": "<p>Salt &amp; pepper</p>"}}),
        );

        let meta = PageMeta::for_item(&item, "/item/x", "https://example.com", "Site", &[]);

        assert_eq!(meta.description.as_deref(), Some("Salt & pepper"));
    }

    #[test]
    fn description_truncates_long_body_text_on_a_word_boundary() {
        let body = "Trovato is a content management system ".repeat(20);
        let item = item("page", serde_json::json!({"field_body": {"value": body}}));

        let meta = PageMeta::for_item(&item, "/item/x", "https://example.com", "Site", &[]);
        let description = meta.description.expect("a description");

        assert!(
            description.len() <= DESCRIPTION_MAX_LEN,
            "description was {} bytes: {description}",
            description.len()
        );
        assert!(description.ends_with("..."), "got: {description}");
        assert!(
            !description.contains("  "),
            "the cut must not leave trailing whitespace before the ellipsis"
        );
    }

    #[test]
    fn description_is_none_when_the_item_has_no_body_text() {
        let item = item("page", serde_json::json!({"field_other": 42}));

        let meta = PageMeta::for_item(&item, "/item/x", "https://example.com", "Site", &[]);

        assert_eq!(
            meta.description, None,
            "an empty description tag is worse than no description tag"
        );
    }

    #[test]
    fn description_falls_back_to_the_first_paragraph_block() {
        let item = item(
            "page",
            serde_json::json!({
                "field_content": [
                    {"type": "header", "data": {"text": "A heading", "level": 2}},
                    {"type": "paragraph", "data": {"text": "The opening <b>paragraph</b>."}},
                    {"type": "paragraph", "data": {"text": "The second paragraph."}},
                ],
            }),
        );
        let fields = [field("field_content", FieldType::Blocks)];

        let meta = PageMeta::for_item(&item, "/item/x", "https://example.com", "Site", &fields);

        assert_eq!(
            meta.description.as_deref(),
            Some("The opening paragraph."),
            "block-editor content types have no field_body"
        );
    }

    /// An array under a field the content type does not declare as Blocks is
    /// data, not content, and must not be mined for a description.
    #[test]
    fn an_undeclared_array_field_is_not_read_as_blocks() {
        let item = item(
            "page",
            serde_json::json!({
                "field_content": [{"type": "paragraph", "data": {"text": "Not content."}}],
            }),
        );

        let meta = PageMeta::for_item(&item, "/item/x", "https://example.com", "Site", &[]);

        assert_eq!(meta.description, None);
    }

    #[test]
    fn og_image_comes_from_the_first_image_block_and_is_absolute() {
        let item = item(
            "page",
            serde_json::json!({
                "field_content": [
                    {"type": "paragraph", "data": {"text": "Text first."}},
                    {"type": "image", "data": {"file": {"url": "/files/lead.jpg"}}},
                    {"type": "image", "data": {"file": {"url": "/files/second.jpg"}}},
                ],
            }),
        );
        let fields = [field("field_content", FieldType::Blocks)];

        let meta = PageMeta::for_item(&item, "/item/x", "https://example.com", "Site", &fields);

        assert_eq!(
            meta.og_image.as_deref(),
            Some("https://example.com/files/lead.jpg")
        );
    }

    #[test]
    fn og_image_reads_the_legacy_flat_block_shape() {
        let item = item(
            "page",
            serde_json::json!({
                "field_content": [{"type": "image", "data": {"url": "/files/legacy.jpg"}}],
            }),
        );
        let fields = [field("field_content", FieldType::Blocks)];

        let meta = PageMeta::for_item(&item, "/item/x", "https://example.com", "Site", &fields);

        assert_eq!(
            meta.og_image.as_deref(),
            Some("https://example.com/files/legacy.jpg")
        );
    }

    #[test]
    fn og_image_hosted_elsewhere_is_left_alone() {
        let item = item(
            "page",
            serde_json::json!({
                "field_content": [
                    {"type": "image", "data": {"file": {"url": "https://cdn.example.net/x.jpg"}}},
                ],
            }),
        );
        let fields = [field("field_content", FieldType::Blocks)];

        let meta = PageMeta::for_item(&item, "/item/x", "https://example.com", "Site", &fields);

        assert_eq!(
            meta.og_image.as_deref(),
            Some("https://cdn.example.net/x.jpg"),
            "an absolute image URL must not be rewritten into this site"
        );
    }

    #[test]
    fn canonical_and_og_url_are_the_same_absolute_url() {
        let item = item("page", serde_json::json!({}));

        let meta = PageMeta::for_item(&item, "/about-us", "https://example.com/", "Site", &[]);

        assert_eq!(
            meta.canonical.as_deref(),
            Some("https://example.com/about-us"),
            "a trailing slash on SITE_URL must not double up"
        );
        assert_eq!(meta.canonical, meta.og_url);
    }

    #[test]
    fn article_types_get_article_og_type_and_timestamps() {
        for item_type in ["blog", "article", "news"] {
            let meta = PageMeta::for_item(
                &item(item_type, serde_json::json!({})),
                "/item/x",
                "https://example.com",
                "Site",
                &[],
            );

            assert_eq!(meta.og_type.as_deref(), Some("article"), "{item_type}");
            assert!(meta.published_time.is_some(), "{item_type}");
            assert!(meta.modified_time.is_some(), "{item_type}");
        }
    }

    #[test]
    fn other_types_are_websites_without_article_timestamps() {
        let meta = PageMeta::for_item(
            &item("page", serde_json::json!({})),
            "/item/x",
            "https://example.com",
            "Site",
            &[],
        );

        assert_eq!(meta.og_type.as_deref(), Some("website"));
        assert_eq!(meta.published_time, None);
        assert_eq!(meta.modified_time, None);
    }

    /// A field-sourced URL is a URL from outside the kernel, so the scheme is
    /// checked rather than assumed. `javascript:` in an `og:image` would be
    /// inert to a crawler but is not something to emit.
    #[test]
    fn a_non_http_image_url_is_dropped() {
        for hostile in ["javascript:alert(1)", "data:text/html,<script>x</script>"] {
            let item = item(
                "page",
                serde_json::json!({
                    "field_content": [{"type": "image", "data": {"url": hostile}}],
                }),
            );
            let fields = [field("field_content", FieldType::Blocks)];

            let meta = PageMeta::for_item(&item, "/item/x", "https://example.com", "Site", &fields);

            assert_eq!(meta.og_image, None, "{hostile} must not be emitted");
        }
    }

    /// The template emits these values unescaped, so the ampersand an HTML
    /// parser would read as an entity is encoded here.
    #[test]
    fn a_query_string_ampersand_is_encoded_for_the_attribute() {
        let item = item(
            "page",
            serde_json::json!({
                "field_content": [
                    {"type": "image", "data": {"url": "/files/x.jpg?w=1200&h=630"}},
                ],
            }),
        );
        let fields = [field("field_content", FieldType::Blocks)];

        let meta = PageMeta::for_item(&item, "/item/x", "https://example.com", "Site", &fields);

        assert_eq!(
            meta.og_image.as_deref(),
            Some("https://example.com/files/x.jpg?w=1200&amp;h=630")
        );
    }

    /// Nothing reaching a URL attribute can carry a quote or an angle bracket,
    /// which is what lets the template emit it with `| safe`.
    #[test]
    fn attribute_breaking_characters_are_percent_encoded() {
        let item = item(
            "page",
            serde_json::json!({
                "field_content": [
                    {"type": "image", "data": {"url": "/files/a\" onerror=\"x().jpg"}},
                ],
            }),
        );
        let fields = [field("field_content", FieldType::Blocks)];

        let meta = PageMeta::for_item(&item, "/item/x", "https://example.com", "Site", &fields);
        let image = meta.og_image.expect("an image URL");

        assert!(!image.contains('"'), "quote survived: {image}");
        assert!(!image.contains('<'), "angle bracket survived: {image}");
        assert!(!image.contains(' '), "space survived: {image}");
        assert!(image.contains("%22"), "the quote must be encoded: {image}");
    }

    #[test]
    fn title_and_site_name_carry_through() {
        let meta = PageMeta::for_item(
            &item("page", serde_json::json!({})),
            "/item/x",
            "https://example.com",
            "My Site",
            &[],
        );

        assert_eq!(meta.og_title.as_deref(), Some("A Title"));
        assert_eq!(meta.og_site_name.as_deref(), Some("My Site"));
    }
}
