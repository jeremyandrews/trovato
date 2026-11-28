//! Text format filter pipeline.
//!
//! Provides security filtering for text content based on format type. The
//! formats are stored (`content::text_format`); the three the kernel ships are
//! seeded with the definitions here:
//! - plain_text: HTML-escapes all content
//! - filtered_html: Allows safe tags, strips dangerous ones
//! - full_html: Trusted rich HTML, cleaned by [`TrustedHtmlFilter`] (a fixed,
//!   permissive ammonia allowlist). Never raw: see [`TrustedHtmlFilter`].

/// Trait for text filters in the pipeline.
pub trait TextFilter: Send + Sync {
    /// Filter name for debugging.
    fn name(&self) -> &str;

    /// Process the input text and return filtered output.
    fn process(&self, input: &str) -> String;
}

/// Pipeline of text filters applied in sequence.
pub struct FilterPipeline {
    filters: Vec<Box<dyn TextFilter>>,
}

impl FilterPipeline {
    /// Create a new empty pipeline.
    pub fn new() -> Self {
        Self {
            filters: Vec::new(),
        }
    }

    /// Add a filter to the pipeline.
    #[allow(clippy::should_implement_trait)] // builder pattern, not arithmetic
    pub fn add<F: TextFilter + 'static>(mut self, filter: F) -> Self {
        self.filters.push(Box::new(filter));
        self
    }

    /// Create pipeline for a specific format, as stored
    /// (`content::text_format`). An unknown format gets `plain_text`, the
    /// safest. Safe to use anywhere, including on a format string that
    /// originates from stored data or plugin input: every pipeline this
    /// builds, including a `none`-filter (`full_html`) format's, is cleaned by
    /// [`TrustedHtmlFilter`] before it is returned.
    pub fn for_format(format: &str) -> Self {
        match super::text_format::get(format) {
            Some(stored) => stored.pipeline(),
            None => Self::plain_text(),
        }
    }

    /// Create pipeline with permission check.
    ///
    /// If `has_full_html` is false and the format is the more permissive,
    /// unconfigured one (`full_html`), downgrades to `filtered_html`'s
    /// configured allowlist instead. Both are sanitized either way: this
    /// gates which allowlist applies, not whether one does.
    pub fn for_format_checked(format: &str, has_full_html: bool) -> Self {
        match super::text_format::get(format) {
            Some(stored)
                if stored.filter == super::text_format::FilterKind::None && !has_full_html =>
            {
                Self::for_format("filtered_html")
            }
            _ => Self::for_format(format),
        }
    }

    /// Create a plain text pipeline (escapes all HTML).
    pub fn plain_text() -> Self {
        Self::new().add(HtmlEscapeFilter).add(NewlineFilter)
    }

    /// Create a filtered HTML pipeline (allows safe tags).
    pub fn filtered_html() -> Self {
        Self::new().add(FilteredHtmlFilter).add(UrlFilter)
    }

    /// Create a full HTML pipeline: rich HTML, cleaned by
    /// [`TrustedHtmlFilter`]'s fixed, permissive allowlist. Never unfiltered.
    pub fn full_html() -> Self {
        Self::new().add(TrustedHtmlFilter)
    }

    /// Process text through all filters in the pipeline.
    pub fn process(&self, input: &str) -> String {
        self.filters
            .iter()
            .fold(input.to_string(), |acc, filter| filter.process(&acc))
    }
}

impl Default for FilterPipeline {
    fn default() -> Self {
        Self::plain_text()
    }
}

/// Filter that escapes all HTML characters.
pub struct HtmlEscapeFilter;

impl TextFilter for HtmlEscapeFilter {
    fn name(&self) -> &str {
        "html_escape"
    }

    fn process(&self, input: &str) -> String {
        input
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#x27;")
    }
}

/// Filter that converts newlines to <br> tags.
pub struct NewlineFilter;

impl TextFilter for NewlineFilter {
    fn name(&self) -> &str {
        "newline"
    }

    fn process(&self, input: &str) -> String {
        input.replace('\n', "<br>\n")
    }
}

/// Filter that allows safe HTML tags and strips dangerous ones using ammonia.
pub struct FilteredHtmlFilter;

impl FilteredHtmlFilter {
    /// List of allowed HTML tags: the code definition `filtered_html` is
    /// seeded with.
    pub(crate) const ALLOWED_TAGS: &'static [&'static str] = &[
        "p",
        "br",
        "strong",
        "b",
        "em",
        "i",
        "u",
        "s",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "ul",
        "ol",
        "li",
        "a",
        "blockquote",
        "pre",
        "code",
        "table",
        "thead",
        "tbody",
        "tr",
        "th",
        "td",
        "img",
        "hr",
    ];

    /// List of allowed attributes per tag.
    ///
    /// Note: "rel" on `<a>` is managed by ammonia's `link_rel()` and must not
    /// appear here — ammonia panics if both are set.
    pub(crate) fn allowed_attributes(tag: &str) -> &'static [&'static str] {
        match tag {
            "a" => &["href", "title", "target"],
            "img" => &["src", "alt", "title", "width", "height"],
            "td" | "th" => &["colspan", "rowspan"],
            _ => &[],
        }
    }

    /// Check if a tag is allowed.
    fn is_allowed_tag(tag: &str) -> bool {
        Self::ALLOWED_TAGS.contains(&tag.to_lowercase().as_str())
    }
}

impl TextFilter for FilteredHtmlFilter {
    fn name(&self) -> &str {
        "filtered_html"
    }

    fn process(&self, input: &str) -> String {
        use std::collections::{HashMap, HashSet};

        let tags: HashSet<&str> = Self::ALLOWED_TAGS.iter().copied().collect();

        let mut tag_attributes: HashMap<&str, HashSet<&str>> = HashMap::new();
        for &tag in Self::ALLOWED_TAGS {
            let attrs = Self::allowed_attributes(tag);
            if !attrs.is_empty() {
                tag_attributes.insert(tag, attrs.iter().copied().collect());
            }
        }

        ammonia::Builder::default()
            .tags(tags)
            .tag_attributes(tag_attributes)
            .link_rel(Some("noopener noreferrer"))
            .clean(input)
            .to_string()
    }
}

/// Filter that keeps only a configured set of tags and attributes: what a
/// stored format with the `html` filter applies. With `filtered_html`'s seeded
/// lists it is [`FilteredHtmlFilter`].
pub struct ConfiguredHtmlFilter {
    tags: Vec<String>,
    attributes: std::collections::BTreeMap<String, Vec<String>>,
}

impl ConfiguredHtmlFilter {
    /// A filter keeping `tags` and, for each, its `attributes`.
    ///
    /// Names ammonia would refuse to build with (`script` and `style`, whose
    /// content it always removes, and `rel` on `<a>`, which it sets itself)
    /// are dropped here as well as refused when a format is saved, so a
    /// format that reached the table some other way cannot panic a render.
    pub fn new(
        tags: &[String],
        attributes: &std::collections::BTreeMap<String, Vec<String>>,
    ) -> Self {
        let tags: Vec<String> = tags
            .iter()
            .map(|t| t.to_ascii_lowercase())
            .filter(|t| !super::text_format::FORBIDDEN_TAGS.contains(&t.as_str()))
            .collect();
        let attributes = attributes
            .iter()
            .filter(|(tag, _)| tags.contains(tag))
            .map(|(tag, attrs)| {
                let attrs: Vec<String> = attrs
                    .iter()
                    .filter(|a| !(tag == "a" && a.as_str() == "rel"))
                    .cloned()
                    .collect();
                (tag.clone(), attrs)
            })
            .filter(|(_, attrs)| !attrs.is_empty())
            .collect();
        Self { tags, attributes }
    }
}

impl TextFilter for ConfiguredHtmlFilter {
    fn name(&self) -> &str {
        "configured_html"
    }

    fn process(&self, input: &str) -> String {
        use std::collections::{HashMap, HashSet};

        let tags: HashSet<&str> = self.tags.iter().map(String::as_str).collect();
        let tag_attributes: HashMap<&str, HashSet<&str>> = self
            .attributes
            .iter()
            .map(|(tag, attrs)| (tag.as_str(), attrs.iter().map(String::as_str).collect()))
            .collect();

        ammonia::Builder::default()
            .tags(tags)
            .tag_attributes(tag_attributes)
            .link_rel(Some("noopener noreferrer"))
            .clean(input)
            .to_string()
    }
}

/// CSS properties kept in a `style` attribute on `full_html` content: enough
/// to reproduce legacy presentational markup (text color and weight, table
/// and cell layout, spacing) without anything that can reach outside the
/// element's box or load or execute remote content (no `position`,
/// `behavior`, `-moz-binding` or `background-image`). Passed to ammonia's
/// `filter_style_properties`, which filters by property name only; value
/// safety is handled separately, see [`strip_dangerous_style_values`].
const TRUSTED_STYLE_PROPERTIES: &[&str] = &[
    "color",
    "background-color",
    "font-family",
    "font-size",
    "font-style",
    "font-weight",
    "text-align",
    "text-decoration",
    "vertical-align",
    "width",
    "height",
    "border",
    "border-collapse",
    "border-color",
    "border-style",
    "border-width",
    "padding",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "margin",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "float",
    "clear",
    "list-style-type",
    "white-space",
];

/// Substrings that make a `style` declaration's value dangerous no matter
/// which allowlisted property carries it: ammonia's `filter_style_properties`
/// only filters by property name, so this is the value-level backstop
/// (checked case-insensitively, and with internal whitespace removed first so
/// `url (...)` or `javascript :` cannot dodge the check by splitting the
/// keyword from its delimiter).
const DANGEROUS_STYLE_VALUE_PATTERNS: &[&str] = &["url(", "expression(", "javascript:", "\\", "/*"];

/// Drops any declaration from a `style` attribute value whose value contains
/// a [`DANGEROUS_STYLE_VALUE_PATTERNS`] substring, keeping the rest. None of
/// `TRUSTED_STYLE_PROPERTIES` legitimately needs a `url(...)`, an old-IE
/// `expression(...)`, a `javascript:` scheme, a backslash escape or a `/*`
/// comment, so a declaration containing one is dropped outright rather than
/// repaired.
fn strip_dangerous_style_values(value: &str) -> String {
    value
        .split(';')
        .map(str::trim)
        .filter(|decl| !decl.is_empty())
        .filter(|decl| {
            let value_part = decl.split_once(':').map_or(*decl, |(_, v)| v);
            let normalized: String = value_part
                .chars()
                .filter(|c| !c.is_whitespace())
                .flat_map(char::to_lowercase)
                .collect();
            !DANGEROUS_STYLE_VALUE_PATTERNS
                .iter()
                .any(|pattern| normalized.contains(pattern))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The ammonia builder [`TrustedHtmlFilter`] cleans with: built once, not per
/// call. Ammonia's default tag and attribute allowlist (ordinary article
/// markup: `div`, `span`, `figure`, `table`, headings, lists, images and the
/// rest), plus:
/// - `class`, `id` and `style` on every tag, `style` kept only to
///   [`TRUSTED_STYLE_PROPERTIES`] with dangerous values stripped by
///   [`strip_dangerous_style_values`]
/// - `target` and `name` on `a`
/// - the `font` tag with `color`, `face` and `size`
/// - `border`, `hspace`, `vspace` and `align` on `img` (ammonia's default
///   already keeps `align`)
/// - `border`, `cellpadding`, `cellspacing`, `width`, `bgcolor` on `table`
///   (ammonia's default already keeps `align`)
/// - `valign`, `width`, `height`, `bgcolor`, `nowrap` on `td` and `th`
///   (ammonia's default already keeps `align`)
///
/// `rel="noopener noreferrer"` is set on every link, using ammonia's default
/// URL schemes. `script` and `style` elements and their content are always
/// removed by ammonia's defaults, as is every `on*` attribute and a
/// `javascript:` or `data:` URL.
static TRUSTED_HTML_BUILDER: std::sync::LazyLock<ammonia::Builder<'static>> =
    std::sync::LazyLock::new(|| {
        let mut builder = ammonia::Builder::default();
        builder
            .add_tags(["font"])
            .add_generic_attributes(["class", "id", "style"])
            .add_tag_attributes("a", ["target", "name"])
            .add_tag_attributes("font", ["color", "face", "size"])
            .add_tag_attributes("img", ["border", "hspace", "vspace"])
            .add_tag_attributes(
                "table",
                ["border", "cellpadding", "cellspacing", "width", "bgcolor"],
            )
            .filter_style_properties(TRUSTED_STYLE_PROPERTIES.iter().copied().collect())
            .attribute_filter(|_element, attribute, value| {
                if attribute == "style" {
                    Some(strip_dangerous_style_values(value).into())
                } else {
                    Some(value.into())
                }
            })
            .link_rel(Some("noopener noreferrer"));
        for tag in ["td", "th"] {
            builder.add_tag_attributes(tag, ["valign", "width", "height", "bgcolor", "nowrap"]);
        }
        builder
    });

/// Filter for `full_html`: rich, trusted HTML cleaned by a fixed, permissive
/// ammonia allowlist. "Trusted" means *rich*, never *raw*: whatever the
/// `use full_html` permission let an author write, this still strips a
/// `<script>`, an event handler and a `javascript:`/`data:` URL, so no stored
/// value — however it reached storage — can ever put one on a page.
pub struct TrustedHtmlFilter;

impl TextFilter for TrustedHtmlFilter {
    fn name(&self) -> &str {
        "trusted_html"
    }

    fn process(&self, input: &str) -> String {
        TRUSTED_HTML_BUILDER.clean(input).to_string()
    }
}

/// Filter that converts URLs to clickable links.
pub struct UrlFilter;

impl TextFilter for UrlFilter {
    fn name(&self) -> &str {
        "url"
    }

    fn process(&self, input: &str) -> String {
        // Simple URL matching - we check context manually
        // Note: This is a simplified approach; a proper implementation would use a parser
        // Infallible: static regex literal, compile cannot fail
        #[allow(clippy::unwrap_used)]
        let url_re = regex::Regex::new(r#"(https?://[^\s<>"']+)"#).unwrap();

        // We'll use a stateful replacement to avoid converting URLs that are already in href/src
        let mut result = String::new();
        let mut last_end = 0;

        for caps in url_re.captures_iter(input) {
            // Infallible: capture group 0 always exists on a regex match
            #[allow(clippy::unwrap_used)]
            let mat = caps.get(0).unwrap();
            let start = mat.start();
            let url = mat.as_str();

            // Check if this URL is already in an href or src attribute
            let prefix = &input[..start];
            let is_in_attr = prefix.ends_with("href=\"")
                || prefix.ends_with("href='")
                || prefix.ends_with("src=\"")
                || prefix.ends_with("src='");

            result.push_str(&input[last_end..start]);

            if is_in_attr {
                // Already in an attribute, keep as-is
                result.push_str(url);
            } else {
                // Convert to link
                result.push_str(&format!(
                    r#"<a href="{url}" target="_blank" rel="noopener">{url}</a>"#
                ));
            }

            last_end = mat.end();
        }

        result.push_str(&input[last_end..]);
        result
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Tests marked `SECURITY REGRESSION TEST` verify fixes for specific security
    //! findings from Epic 27. Do not remove without security review.

    use super::*;

    // SECURITY REGRESSION TEST — Story 27.1 (XSS): HTML escaping prevents script injection
    #[test]
    fn html_escape_filter() {
        let filter = HtmlEscapeFilter;
        assert_eq!(
            filter.process("<script>alert('xss')</script>"),
            "&lt;script&gt;alert(&#x27;xss&#x27;)&lt;/script&gt;"
        );
    }

    #[test]
    fn newline_filter() {
        let filter = NewlineFilter;
        assert_eq!(filter.process("line1\nline2"), "line1<br>\nline2");
    }

    // SECURITY REGRESSION TEST — Story 27.1 (XSS): ammonia strips <script> tags
    #[test]
    fn filtered_html_removes_scripts() {
        let filter = FilteredHtmlFilter;
        let input = "<p>Safe</p><script>alert('xss')</script><p>Also safe</p>";
        let output = filter.process(input);
        assert!(!output.contains("script"));
        assert!(output.contains("<p>Safe</p>"));
    }

    // SECURITY REGRESSION TEST — Story 27.1 (XSS): ammonia strips event handlers
    #[test]
    fn filtered_html_removes_event_handlers() {
        let filter = FilteredHtmlFilter;
        let input = r#"<a href="/page" onclick="alert('xss')">Link</a>"#;
        let output = filter.process(input);
        assert!(!output.contains("onclick"));
    }

    // SECURITY REGRESSION TEST — Story 27.1 (XSS): ammonia blocks javascript: URIs
    #[test]
    fn filtered_html_removes_javascript_urls() {
        let filter = FilteredHtmlFilter;
        let input = r#"<a href="javascript:alert('xss')">Link</a>"#;
        let output = filter.process(input);
        assert!(!output.contains("javascript:"));
    }

    #[test]
    fn url_filter_converts_urls() {
        let filter = UrlFilter;
        let input = "Check out https://example.com for more info.";
        let output = filter.process(input);
        assert!(output.contains(r#"<a href="https://example.com""#));
    }

    #[test]
    fn plain_text_pipeline() {
        let pipeline = FilterPipeline::plain_text();
        let input = "<script>alert('xss')</script>\nLine 2";
        let output = pipeline.process(input);
        assert!(!output.contains("<script>"));
        assert!(output.contains("<br>"));
    }

    #[test]
    fn filtered_html_pipeline() {
        let pipeline = FilterPipeline::filtered_html();
        let input = "<p>Hello</p><script>bad</script>";
        let output = pipeline.process(input);
        assert!(output.contains("<p>Hello</p>"));
        assert!(!output.contains("script"));
    }

    // SECURITY REGRESSION TEST — B151: full_html is cleaned, never raw
    #[test]
    fn full_html_pipeline_strips_script_and_style() {
        let pipeline = FilterPipeline::full_html();
        let input = "<script>alert('test')</script><style>body{}</style><p>Safe</p>";
        let output = pipeline.process(input);
        assert!(!output.contains("script"));
        assert!(!output.contains("style"));
        assert!(output.contains("<p>Safe</p>"));
    }

    #[test]
    fn filter_pipeline_default() {
        let pipeline = FilterPipeline::default();
        // Default should be plain_text (safest)
        let input = "<b>bold</b>";
        let output = pipeline.process(input);
        assert!(output.contains("&lt;b&gt;"));
    }

    #[test]
    fn filter_pipeline_for_unknown_format() {
        let pipeline = FilterPipeline::for_format("nonexistent");
        // Unknown format defaults to plain_text
        let input = "<i>italic</i>";
        let output = pipeline.process(input);
        assert!(output.contains("&lt;i&gt;"));
    }

    #[test]
    fn filtered_html_removes_onload() {
        let filter = FilteredHtmlFilter;
        let input = r#"<body onload="alert('xss')">content</body>"#;
        let output = filter.process(input);
        assert!(!output.contains("onload"));
    }

    #[test]
    fn filtered_html_removes_onerror() {
        let filter = FilteredHtmlFilter;
        let input = r#"<img src="x" onerror="alert('xss')">"#;
        let output = filter.process(input);
        assert!(!output.contains("onerror"));
    }

    #[test]
    fn url_filter_preserves_non_url_text() {
        let filter = UrlFilter;
        let input = "No URLs here, just plain text.";
        let output = filter.process(input);
        assert_eq!(input, output);
    }

    #[test]
    fn url_filter_http_url() {
        let filter = UrlFilter;
        let input = "Visit http://example.com for info.";
        let output = filter.process(input);
        assert!(output.contains(r#"<a href="http://example.com""#));
    }

    #[test]
    fn html_escape_all_chars() {
        let filter = HtmlEscapeFilter;
        let input = "<>&\"'";
        let output = filter.process(input);
        assert_eq!(output, "&lt;&gt;&amp;&quot;&#x27;");
    }

    #[test]
    fn filter_names() {
        assert_eq!(HtmlEscapeFilter.name(), "html_escape");
        assert_eq!(NewlineFilter.name(), "newline");
        assert_eq!(FilteredHtmlFilter.name(), "filtered_html");
        assert_eq!(UrlFilter.name(), "url");
    }

    #[test]
    fn filtered_html_preserves_allowed_attrs() {
        let filter = FilteredHtmlFilter;
        let input = r#"<a href="https://example.com" title="Example">Link</a>"#;
        let output = filter.process(input);
        assert!(output.contains(r#"href="https://example.com""#));
        assert!(output.contains(r#"title="Example""#));
    }

    #[test]
    fn filtered_html_strips_disallowed_attrs() {
        let filter = FilteredHtmlFilter;
        let input = r#"<p class="fancy" style="color:red">Text</p>"#;
        let output = filter.process(input);
        assert!(!output.contains("class"));
        assert!(!output.contains("style"));
        assert!(output.contains("<p>Text</p>"));
    }

    // SECURITY REGRESSION TEST — Story 27.1 (XSS): ammonia strips iframe and div
    #[test]
    fn filtered_html_strips_disallowed_tags() {
        let filter = FilteredHtmlFilter;
        let input = "<div><p>Keep</p><iframe src='evil'></iframe></div>";
        let output = filter.process(input);
        assert!(output.contains("<p>Keep</p>"));
        assert!(!output.contains("iframe"));
        assert!(!output.contains("div"));
    }

    // SECURITY REGRESSION TEST — Story 27.1 (XSS): complex XSS vectors (SVG, data URI)
    #[test]
    fn filtered_html_complex_xss_payloads() {
        let filter = FilteredHtmlFilter;

        // SVG-based XSS
        let input = r#"<svg onload="alert(1)"><circle r="50"/></svg>"#;
        let output = filter.process(input);
        assert!(!output.contains("svg"));
        assert!(!output.contains("onload"));
        assert!(!output.contains("alert"));

        // Nested script in attributes
        let input = r#"<img src=x onerror="alert('xss')">"#;
        let output = filter.process(input);
        assert!(!output.contains("onerror"));

        // data: URI
        let input = r#"<a href="data:text/html,<script>alert(1)</script>">click</a>"#;
        let output = filter.process(input);
        assert!(!output.contains("data:"));

        // javascript: URI
        let input = r#"<a href="javascript:alert(1)">click</a>"#;
        let output = filter.process(input);
        assert!(!output.contains("javascript:"));
    }

    #[test]
    fn filtered_html_img_attrs_preserved() {
        let filter = FilteredHtmlFilter;
        let input = r#"<img src="photo.jpg" alt="A photo" width="200" height="100">"#;
        let output = filter.process(input);
        assert!(output.contains(r#"src="photo.jpg""#));
        assert!(output.contains(r#"alt="A photo""#));
        assert!(output.contains(r#"width="200""#));
        assert!(output.contains(r#"height="100""#));
    }

    #[test]
    fn filtered_html_adds_link_rel() {
        let filter = FilteredHtmlFilter;
        let input = r#"<a href="https://example.com">Link</a>"#;
        let output = filter.process(input);
        assert!(output.contains("noopener"));
        assert!(output.contains("noreferrer"));
    }

    #[test]
    fn filtered_html_td_th_attrs() {
        let filter = FilteredHtmlFilter;
        let input =
            r#"<table><tr><td colspan="2">Cell</td><th rowspan="3">Header</th></tr></table>"#;
        let output = filter.process(input);
        assert!(output.contains(r#"colspan="2""#));
        assert!(output.contains(r#"rowspan="3""#));
    }

    // SECURITY REGRESSION TEST — Story 27.1 (XSS): ammonia strips <style> tags
    #[test]
    fn filtered_html_removes_style_tags() {
        let filter = FilteredHtmlFilter;
        let input = "<style>body { display:none }</style><p>Visible</p>";
        let output = filter.process(input);
        assert!(!output.contains("style"));
        assert!(output.contains("<p>Visible</p>"));
    }

    #[test]
    fn is_allowed_tag_case_insensitive() {
        assert!(FilteredHtmlFilter::is_allowed_tag("P"));
        assert!(FilteredHtmlFilter::is_allowed_tag("A"));
        assert!(!FilteredHtmlFilter::is_allowed_tag("SCRIPT"));
    }

    #[test]
    fn for_format_checked_downgrades_full_html() {
        let pipeline = FilterPipeline::for_format_checked("full_html", false);
        // Should downgrade: script tags should be stripped (filtered_html behavior)
        let input = "<script>alert('xss')</script><p>Safe</p>";
        let output = pipeline.process(input);
        assert!(!output.contains("script"));
        assert!(output.contains("<p>Safe</p>"));
    }

    // SECURITY REGRESSION TEST — B151: permitted full_html is still cleaned
    #[test]
    fn for_format_checked_allows_full_html_when_permitted() {
        let pipeline = FilterPipeline::for_format_checked("full_html", true);
        let input = r#"<p class="lead">Hi</p><script>alert('test')</script>"#;
        let output = pipeline.process(input);
        assert!(!output.contains("script"));
        assert!(output.contains(r#"<p class="lead">Hi</p>"#));
    }

    #[test]
    fn for_format_checked_plain_text_unaffected() {
        let pipeline = FilterPipeline::for_format_checked("plain_text", false);
        let input = "<b>bold</b>";
        let output = pipeline.process(input);
        assert!(output.contains("&lt;b&gt;"));
    }

    // SECURITY REGRESSION TEST — Story 27.1 Finding R2-1, updated for B151:
    // full_html is never raw — it is cleaned by TrustedHtmlFilter, not
    // downgraded to plain_text, but a script still never survives.
    #[test]
    fn for_format_rejects_script_in_full_html() {
        let pipeline = FilterPipeline::for_format("full_html");
        let input = r#"<p class="lead">Hi</p><script>alert('xss')</script>"#;
        let output = pipeline.process(input);
        assert!(!output.contains("<script"));
        assert!(output.contains(r#"<p class="lead">Hi</p>"#));
    }

    // SECURITY REGRESSION TEST — Story 27.1 Finding R2-1: plain_text allowed
    #[test]
    fn for_format_allows_plain_text() {
        let pipeline = FilterPipeline::for_format("plain_text");
        let input = "<b>bold</b>";
        let output = pipeline.process(input);
        assert!(output.contains("&lt;b&gt;"));
    }

    // SECURITY REGRESSION TEST — Story 27.1 Finding R2-1: filtered_html allowed
    #[test]
    fn for_format_allows_filtered_html() {
        let pipeline = FilterPipeline::for_format("filtered_html");
        let input = "<p>Hello</p><script>bad</script>";
        let output = pipeline.process(input);
        assert!(output.contains("<p>Hello</p>"));
        assert!(!output.contains("script"));
    }

    // SECURITY REGRESSION TEST — Story 27.1 Finding R2-1: unknown formats downgraded
    #[test]
    fn for_format_rejects_unknown() {
        let pipeline = FilterPipeline::for_format("evil_format");
        let input = "<b>bold</b>";
        let output = pipeline.process(input);
        assert!(output.contains("&lt;b&gt;"));
    }

    #[test]
    fn empty_pipeline_is_passthrough() {
        let pipeline = FilterPipeline::new();
        let input = "<script>alert('xss')</script>";
        let output = pipeline.process(input);
        assert_eq!(input, output);
    }

    #[test]
    fn pipeline_chains_filters_in_order() {
        // plain_text = HtmlEscape then Newline
        // HtmlEscape converts < to &lt;, then Newline converts \n to <br>\n
        let pipeline = FilterPipeline::plain_text();
        let input = "<b>bold</b>\nnext";
        let output = pipeline.process(input);
        // After escape: "&lt;b&gt;bold&lt;/b&gt;\nnext"
        // After newline: "&lt;b&gt;bold&lt;/b&gt;<br>\nnext"
        assert!(output.contains("&lt;b&gt;"));
        assert!(output.contains("<br>\n"));
    }

    #[test]
    fn url_filter_skips_urls_in_href() {
        let filter = UrlFilter;
        let input = r#"<a href="https://example.com">link</a>"#;
        let output = filter.process(input);
        // Should NOT double-wrap the URL since it's already in href
        assert!(!output.contains(r#"<a href="<a href="#));
        assert!(output.contains(r#"href="https://example.com""#));
    }

    #[test]
    fn url_filter_skips_urls_in_src() {
        let filter = UrlFilter;
        let input = r#"<img src="https://example.com/photo.jpg">"#;
        let output = filter.process(input);
        assert!(!output.contains(r#"<a href="https://example.com/photo.jpg""#));
    }

    #[test]
    fn filtered_html_preserves_table_structure() {
        let filter = FilteredHtmlFilter;
        let input = "<table><thead><tr><th>Header</th></tr></thead><tbody><tr><td>Cell</td></tr></tbody></table>";
        let output = filter.process(input);
        assert!(output.contains("<table>"));
        assert!(output.contains("<thead>"));
        assert!(output.contains("<tbody>"));
        assert!(output.contains("<tr>"));
        assert!(output.contains("<th>"));
        assert!(output.contains("<td>"));
    }

    #[test]
    fn filtered_html_preserves_text_formatting() {
        let filter = FilteredHtmlFilter;
        let input = "<p><strong>Bold</strong> and <em>italic</em> and <u>underline</u> and <s>strike</s></p>";
        let output = filter.process(input);
        assert!(output.contains("<strong>Bold</strong>"));
        assert!(output.contains("<em>italic</em>"));
        assert!(output.contains("<u>underline</u>"));
        assert!(output.contains("<s>strike</s>"));
    }

    #[test]
    fn filtered_html_preserves_headings() {
        let filter = FilteredHtmlFilter;
        for level in 1..=6 {
            let input = format!("<h{level}>Heading {level}</h{level}>");
            let output = filter.process(&input);
            assert!(
                output.contains(&format!("<h{level}>")),
                "h{level} not preserved"
            );
        }
    }

    #[test]
    fn for_format_routes_correctly() {
        // Verify each named format routes to the expected pipeline behavior
        let plain = FilterPipeline::for_format("plain_text");
        assert!(plain.process("<b>x</b>").contains("&lt;b&gt;"));

        let filtered = FilterPipeline::for_format("filtered_html");
        assert!(
            filtered
                .process("<p>ok</p><script>bad</script>")
                .contains("<p>ok</p>")
        );

        let full = FilterPipeline::for_format("full_html");
        let full_output = full.process(r#"<p class="lead">ok</p><script>bad</script>"#);
        assert!(full_output.contains(r#"<p class="lead">ok</p>"#));
        assert!(!full_output.contains("script"));
    }

    // -----------------------------------------------------------------------
    // TrustedHtmlFilter (B151): full_html is rich, never raw.
    // -----------------------------------------------------------------------

    // SECURITY REGRESSION TEST — B151: script element and its content removed
    #[test]
    fn trusted_html_removes_script() {
        let filter = TrustedHtmlFilter;
        let output = filter.process("<p>Safe</p><script>alert('xss')</script>");
        assert!(!output.contains("script"));
        assert!(!output.contains("alert"));
        assert!(output.contains("<p>Safe</p>"));
    }

    // SECURITY REGRESSION TEST — B151: style element and its content removed
    #[test]
    fn trusted_html_removes_style_element() {
        let filter = TrustedHtmlFilter;
        let output = filter.process("<style>body{display:none}</style><p>Visible</p>");
        assert!(!output.contains("style"));
        assert!(!output.contains("display"));
        assert!(output.contains("<p>Visible</p>"));
    }

    // B308: an allowlisted style declaration survives
    #[test]
    fn trusted_html_keeps_allowlisted_style() {
        let filter = TrustedHtmlFilter;
        let output = filter.process(r#"<p style="color:red">Text</p>"#);
        assert!(output.contains(r#"style="color:red""#), "{output}");
        assert!(output.contains("Text"));
    }

    // B308: multiple allowlisted declarations survive together
    #[test]
    fn trusted_html_keeps_multiple_allowlisted_style_declarations() {
        let filter = TrustedHtmlFilter;
        let output =
            filter.process(r#"<p style="color: red; text-align: center; width: 50%">Text</p>"#);
        assert!(output.contains("color:red"), "{output}");
        assert!(output.contains("text-align:center"), "{output}");
        assert!(output.contains("width:50%"), "{output}");
    }

    // B308: a style property not on the allowlist is dropped, the rest kept
    #[test]
    fn trusted_html_strips_non_allowlisted_style_properties() {
        let filter = TrustedHtmlFilter;
        for property in ["position", "behavior", "-moz-binding", "background-image"] {
            let input = format!(r#"<p style="color: red; {property}: evil">Text</p>"#);
            let output = filter.process(&input);
            assert!(!output.contains(property), "{property} survived: {output}");
            assert!(output.contains("color:red"), "{property}: {output}");
        }
    }

    // B308: a declaration whose value smuggles a URL, expression, javascript:
    // scheme, backslash escape or comment is dropped even on an allowlisted
    // property
    #[test]
    fn trusted_html_strips_dangerous_style_values() {
        let filter = TrustedHtmlFilter;
        for value in [
            "color: url(evil.css)",
            "width: expression(alert(1))",
            "color: javascript:alert(1)",
            r"width: 1\65 x",
            "color: red/*comment*/",
        ] {
            let input = format!(r#"<p style="{value}">Text</p>"#);
            let output = filter.process(&input);
            assert!(
                !output.contains("url(")
                    && !output.contains("expression(")
                    && !output.contains("javascript:"),
                "{value} survived: {output}"
            );
            assert!(output.contains("Text"));
        }
    }

    #[test]
    fn strip_dangerous_style_values_keeps_safe_declarations() {
        let output = strip_dangerous_style_values("color: red; text-align: center");
        assert_eq!(output, "color: red; text-align: center");
    }

    #[test]
    fn strip_dangerous_style_values_drops_only_the_bad_declaration() {
        let output = strip_dangerous_style_values("color: red; width: expression(alert(1))");
        assert_eq!(output, "color: red");
    }

    #[test]
    fn strip_dangerous_style_values_case_insensitive() {
        let output = strip_dangerous_style_values("color: JAVASCRIPT:alert(1)");
        assert_eq!(output, "");
    }

    // B308: legacy presentational tags and attributes survive full_html
    #[test]
    fn trusted_html_keeps_legacy_presentational_markup() {
        let filter = TrustedHtmlFilter;

        let font = filter.process(r#"<font color="red" face="Arial" size="4">Q:</font>"#);
        assert!(font.contains(r#"color="red""#));
        assert!(font.contains(r#"face="Arial""#));
        assert!(font.contains(r#"size="4""#));

        let anchor = filter.process(r#"<a name="section1">Jump</a>"#);
        assert!(anchor.contains(r#"name="section1""#));

        let img =
            filter.process(r#"<img src="/a.png" border="1" hspace="5" vspace="5" align="left">"#);
        assert!(img.contains(r#"border="1""#));
        assert!(img.contains(r#"hspace="5""#));
        assert!(img.contains(r#"vspace="5""#));
        assert!(img.contains(r#"align="left""#));

        let table = filter.process(
            r#"<table border="1" cellpadding="2" cellspacing="3" width="100%" align="center" bgcolor="white"><tr><th valign="middle" width="30" height="15" bgcolor="gray" nowrap>Head</th><td align="left" valign="top" width="50" height="20" bgcolor="silver" nowrap style="color: red; text-align: center">Cell</td></tr></table>"#,
        );
        assert!(table.contains(r#"border="1""#));
        assert!(table.contains(r#"cellpadding="2""#));
        assert!(table.contains(r#"cellspacing="3""#));
        assert!(table.contains(r#"width="100%""#));
        assert!(table.contains(r#"align="center""#));
        assert!(table.contains(r#"bgcolor="white""#));
        assert!(table.contains(r#"valign="middle""#), "th valign: {table}");
        assert!(table.contains(r#"width="30""#), "th width: {table}");
        assert!(table.contains(r#"height="15""#), "th height: {table}");
        assert!(table.contains(r#"bgcolor="gray""#), "th bgcolor: {table}");
        assert!(table.contains(r#"valign="top""#), "td valign: {table}");
        assert!(table.contains(r#"width="50""#), "td width: {table}");
        assert!(table.contains(r#"height="20""#), "td height: {table}");
        assert!(table.contains(r#"bgcolor="silver""#), "td bgcolor: {table}");
        assert!(table.contains("nowrap"));
        assert!(table.contains("color:red"));
        assert!(table.contains("text-align:center"));
    }

    // B308: an empty style attribute is harmless and does not panic
    #[test]
    fn trusted_html_keeps_empty_style_attribute() {
        let filter = TrustedHtmlFilter;
        let output = filter.process(r#"<p style="">Text</p>"#);
        assert!(output.contains("Text"));
    }

    // B308: a declaration with only whitespace between `url` and `(` is still
    // caught by the value backstop, not just an exact `url(` substring
    #[test]
    fn strip_dangerous_style_values_catches_whitespace_obfuscation() {
        let output = strip_dangerous_style_values("color: url (evil.png)");
        assert_eq!(output, "");
        let output = strip_dangerous_style_values("color: javascript : alert(1)");
        assert_eq!(output, "");
    }

    // SECURITY REGRESSION TEST — B151: every on* handler is stripped
    #[test]
    fn trusted_html_removes_event_handlers() {
        let filter = TrustedHtmlFilter;
        for (tag_html, handler) in [
            (r#"<div onclick="x()">click</div>"#, "onclick"),
            (r#"<img src="/a.png" onerror="x()">"#, "onerror"),
            (r#"<body onload="x()">hi</body>"#, "onload"),
            (r#"<input onfocus="x()">"#, "onfocus"),
            (r#"<a href="/" onmouseover="x()">link</a>"#, "onmouseover"),
        ] {
            let output = filter.process(tag_html);
            assert!(!output.contains(handler), "{handler} survived: {output}");
        }
    }

    // SECURITY REGRESSION TEST — B151: javascript: and data: URLs are blocked
    #[test]
    fn trusted_html_removes_javascript_and_data_urls() {
        let filter = TrustedHtmlFilter;
        let js = filter.process(r#"<a href="javascript:alert(1)">click</a>"#);
        assert!(!js.contains("javascript:"));

        let data =
            filter.process(r#"<a href="data:text/html,<script>alert(1)</script>">click</a>"#);
        assert!(!data.contains("data:"));
    }

    // SECURITY REGRESSION TEST — B151: SVG with onload is neutralised
    #[test]
    fn trusted_html_removes_svg_onload() {
        let filter = TrustedHtmlFilter;
        let output = filter.process(r#"<svg onload="alert(1)"><circle r="50"/></svg>"#);
        assert!(!output.contains("onload"));
        assert!(!output.contains("alert"));
    }

    // SECURITY REGRESSION TEST — B151: iframe is not in the trusted allowlist
    #[test]
    fn trusted_html_removes_iframe() {
        let filter = TrustedHtmlFilter;
        let output = filter.process("<p>Keep</p><iframe src=\"https://evil.example\"></iframe>");
        assert!(output.contains("<p>Keep</p>"));
        assert!(!output.contains("iframe"));
    }

    // An unclosed tag does not panic and does not leak the rest of the
    // document as unescaped text.
    #[test]
    fn trusted_html_handles_unclosed_tag() {
        let filter = TrustedHtmlFilter;
        let output = filter.process("<p>Unclosed<script>alert(1)");
        assert!(!output.contains("script"));
        assert!(!output.contains("alert"));
    }

    #[test]
    fn trusted_html_preserves_rich_markup() {
        let filter = TrustedHtmlFilter;
        let input = r#"<div class="article" id="main"><h2>Title</h2><figure><img src="/a.png" alt="A"></figure><table><tr><td>Cell</td></tr></table><span class="tag">x</span></div>"#;
        let output = filter.process(input);
        assert!(output.contains(r#"<div class="article" id="main">"#));
        assert!(output.contains("<h2>Title</h2>"));
        assert!(output.contains("<figure>"));
        assert!(output.contains(r#"<img src="/a.png" alt="A">"#) || output.contains("<img"));
        assert!(output.contains("<table>"));
        assert!(output.contains(r#"<span class="tag">x</span>"#));
    }

    #[test]
    fn trusted_html_adds_link_rel_and_keeps_target() {
        let filter = TrustedHtmlFilter;
        let output = filter.process(r#"<a href="https://example.com" target="_blank">Link</a>"#);
        assert!(output.contains("noopener"));
        assert!(output.contains("noreferrer"));
        assert!(output.contains(r#"target="_blank""#));
    }

    #[test]
    fn trusted_html_name() {
        assert_eq!(TrustedHtmlFilter.name(), "trusted_html");
    }
}
