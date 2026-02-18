//! Text format filter pipeline.
//!
//! Provides security filtering for text content based on format type:
//! - plain_text: HTML-escapes all content
//! - filtered_html: Allows safe tags, strips dangerous ones
//! - full_html: No filtering (admin only)

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
    pub fn add<F: TextFilter + 'static>(mut self, filter: F) -> Self {
        self.filters.push(Box::new(filter));
        self
    }

    /// Create pipeline for a specific format.
    pub fn for_format(format: &str) -> Self {
        match format {
            "plain_text" => Self::plain_text(),
            "filtered_html" => Self::filtered_html(),
            "full_html" => Self::full_html(),
            _ => Self::plain_text(), // Default to safest option
        }
    }

    /// Create pipeline with permission check.
    ///
    /// If `has_full_html` is false and format is "full_html", downgrades to
    /// filtered_html for safety.
    pub fn for_format_checked(format: &str, has_full_html: bool) -> Self {
        match format {
            "full_html" if !has_full_html => Self::filtered_html(),
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

    /// Create a full HTML pipeline (no filtering - admin only).
    pub fn full_html() -> Self {
        Self::new()
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
    /// List of allowed HTML tags.
    const ALLOWED_TAGS: &'static [&'static str] = &[
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
    fn allowed_attributes(tag: &str) -> &'static [&'static str] {
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

/// Filter that converts URLs to clickable links.
pub struct UrlFilter;

impl TextFilter for UrlFilter {
    fn name(&self) -> &str {
        "url"
    }

    fn process(&self, input: &str) -> String {
        // Simple URL matching - we check context manually
        // Note: This is a simplified approach; a proper implementation would use a parser
        let url_re = regex::Regex::new(r#"(https?://[^\s<>"']+)"#).unwrap();

        // We'll use a stateful replacement to avoid converting URLs that are already in href/src
        let mut result = String::new();
        let mut last_end = 0;

        for caps in url_re.captures_iter(input) {
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
                    r#"<a href="{}" target="_blank" rel="noopener">{}</a>"#,
                    url, url
                ));
            }

            last_end = mat.end();
        }

        result.push_str(&input[last_end..]);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn filtered_html_removes_scripts() {
        let filter = FilteredHtmlFilter;
        let input = "<p>Safe</p><script>alert('xss')</script><p>Also safe</p>";
        let output = filter.process(input);
        assert!(!output.contains("script"));
        assert!(output.contains("<p>Safe</p>"));
    }

    #[test]
    fn filtered_html_removes_event_handlers() {
        let filter = FilteredHtmlFilter;
        let input = r#"<a href="/page" onclick="alert('xss')">Link</a>"#;
        let output = filter.process(input);
        assert!(!output.contains("onclick"));
    }

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

    #[test]
    fn full_html_pipeline_no_filtering() {
        let pipeline = FilterPipeline::full_html();
        let input = "<script>alert('test')</script><style>body{}</style>";
        let output = pipeline.process(input);
        assert_eq!(input, output);
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

    #[test]
    fn filtered_html_strips_disallowed_tags() {
        let filter = FilteredHtmlFilter;
        let input = "<div><p>Keep</p><iframe src='evil'></iframe></div>";
        let output = filter.process(input);
        assert!(output.contains("<p>Keep</p>"));
        assert!(!output.contains("iframe"));
        assert!(!output.contains("div"));
    }

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

    #[test]
    fn for_format_checked_allows_full_html_when_permitted() {
        let pipeline = FilterPipeline::for_format_checked("full_html", true);
        let input = "<script>alert('test')</script>";
        let output = pipeline.process(input);
        assert_eq!(input, output);
    }

    #[test]
    fn for_format_checked_plain_text_unaffected() {
        let pipeline = FilterPipeline::for_format_checked("plain_text", false);
        let input = "<b>bold</b>";
        let output = pipeline.process(input);
        assert!(output.contains("&lt;b&gt;"));
    }
}
