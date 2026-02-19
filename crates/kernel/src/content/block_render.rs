//! Server-side block rendering for Editor.js content (Epic 24.5, 24.6, 24.7).
//!
//! Converts Editor.js block JSON into semantic HTML:
//! - Paragraph, heading, image, list, quote, code, delimiter, embed
//! - Code blocks use `syntect` for syntax highlighting (24.6)
//! - Embed blocks enforce a URL whitelist for iframe rendering (24.7)

use serde_json::Value;
use std::sync::LazyLock;

use crate::routes::helpers::html_escape;

/// Sanitize user-provided rich text, allowing only safe inline HTML.
///
/// Uses ammonia to strip dangerous tags/attributes while preserving
/// basic formatting tags (`<b>`, `<i>`, `<a>`, `<br>`, etc.).
fn sanitize_text(input: &str) -> String {
    ammonia::clean(input)
}

/// Validate that a URL uses a safe scheme (http or https).
fn is_safe_url(url: &str) -> bool {
    let trimmed = url.trim();
    trimmed.starts_with("https://") || trimmed.starts_with("http://")
}

// Pre-loaded syntect resources (Finding 13: avoid reloading per call).
static SYNTAX_SET: LazyLock<syntect::parsing::SyntaxSet> =
    LazyLock::new(syntect::parsing::SyntaxSet::load_defaults_newlines);
static THEME_SET: LazyLock<syntect::highlighting::ThemeSet> =
    LazyLock::new(syntect::highlighting::ThemeSet::load_defaults);

/// Render an array of Editor.js blocks into a single HTML string.
///
/// Each block is expected to be a JSON object with at minimum a `"type"` field
/// and a `"data"` object. Unknown block types are silently skipped.
pub fn render_blocks(blocks: &[Value]) -> String {
    let mut html = String::new();
    for block in blocks {
        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let data = block
            .get("data")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let rendered = match block_type {
            "paragraph" => render_paragraph(&data),
            "heading" | "header" => render_heading(&data),
            "image" => render_image(&data),
            "list" => render_list(&data),
            "quote" => render_quote(&data),
            "code" => render_code(&data),
            "delimiter" => render_delimiter(),
            "embed" => render_embed(&data),
            _ => String::new(),
        };
        html.push_str(&rendered);
    }
    html
}

// ---------------------------------------------------------------------------
// Individual block renderers
// ---------------------------------------------------------------------------

/// Render a paragraph block.
/// Data: `{ "text": "..." }`
fn render_paragraph(data: &Value) -> String {
    let text = data.get("text").and_then(|v| v.as_str()).unwrap_or("");
    format!("<p>{}</p>", sanitize_text(text))
}

/// Render a heading block.
/// Data: `{ "text": "...", "level": 2 }`
fn render_heading(data: &Value) -> String {
    let text = data.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let level = data
        .get("level")
        .and_then(|v| v.as_u64())
        .unwrap_or(2)
        .clamp(1, 6);
    let clean = sanitize_text(text);
    format!("<h{level}>{clean}</h{level}>")
}

/// Render an image block with a figure/figcaption wrapper.
/// Data: `{ "file": { "url": "..." }, "caption": "..." }` or `{ "url": "...", "caption": "..." }`
fn render_image(data: &Value) -> String {
    let url = data
        .get("file")
        .and_then(|f| f.get("url"))
        .and_then(|v| v.as_str())
        .or_else(|| data.get("url").and_then(|v| v.as_str()))
        .unwrap_or("");
    let caption = data.get("caption").and_then(|v| v.as_str()).unwrap_or("");
    let escaped_url = html_escape(url);
    let escaped_caption = html_escape(caption);
    format!(
        "<figure><img src=\"{escaped_url}\" alt=\"{escaped_caption}\">\
         <figcaption>{escaped_caption}</figcaption></figure>"
    )
}

/// Render a list block (ordered or unordered).
/// Data: `{ "style": "ordered"|"unordered", "items": ["...", ...] }`
fn render_list(data: &Value) -> String {
    let style = data
        .get("style")
        .and_then(|v| v.as_str())
        .unwrap_or("unordered");
    let tag = if style == "ordered" { "ol" } else { "ul" };

    let items = data.get("items").and_then(|v| v.as_array());
    let mut html = format!("<{tag}>");
    if let Some(items) = items {
        for item in items {
            // Items can be plain strings or objects with a "content" field
            let content = item
                .as_str()
                .or_else(|| item.get("content").and_then(|v| v.as_str()))
                .unwrap_or("");
            html.push_str(&format!("<li>{}</li>", sanitize_text(content)));
        }
    }
    html.push_str(&format!("</{tag}>"));
    html
}

/// Render a quote block.
/// Data: `{ "text": "...", "caption": "..." }`
fn render_quote(data: &Value) -> String {
    let text = data.get("text").and_then(|v| v.as_str()).unwrap_or("");
    let caption = data.get("caption").and_then(|v| v.as_str()).unwrap_or("");
    let clean_text = sanitize_text(text);
    let clean_caption = sanitize_text(caption);
    if clean_caption.is_empty() {
        format!("<blockquote><p>{clean_text}</p></blockquote>")
    } else {
        format!("<blockquote><p>{clean_text}</p><cite>{clean_caption}</cite></blockquote>")
    }
}

/// Render a code block with syntax highlighting via `syntect` (Epic 24.6).
/// Data: `{ "code": "...", "language": "rust" }`
///
/// Uses the "InspiredGitHub" theme with a fallback to "base16-ocean.dark".
/// If the language is unknown or not specified, the code is rendered as
/// HTML-escaped plain text.
///
/// # Panics
///
/// Panics if syntect ships without any built-in themes. The function tries
/// "InspiredGitHub" then "base16-ocean.dark"; both are included in the
/// default `ThemeSet::load_defaults()`.
fn render_code(data: &Value) -> String {
    let code = data.get("code").and_then(|v| v.as_str()).unwrap_or("");
    let lang = data
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    if lang.is_empty() {
        return format!("<pre><code>{}</code></pre>", html_escape(code));
    }

    // Attempt syntax highlighting (using pre-loaded statics)
    let ss = &*SYNTAX_SET;
    let ts = &*THEME_SET;

    let syntax = ss
        .find_syntax_by_token(lang)
        .or_else(|| ss.find_syntax_by_name(lang));

    let Some(syntax) = syntax else {
        // Unknown language — fall back to plain escaped text
        return format!("<pre><code>{}</code></pre>", html_escape(code));
    };

    // syntect ships built-in themes; falls back through known names
    #[allow(clippy::expect_used)]
    let theme = ts
        .themes
        .get("InspiredGitHub")
        .or_else(|| ts.themes.get("base16-ocean.dark"))
        .expect("syntect must ship at least one default theme");

    match syntect::html::highlighted_html_for_string(code, ss, syntax, theme) {
        Ok(highlighted) => {
            format!(
                "<pre><code class=\"language-{}\">{}</code></pre>",
                html_escape(lang),
                highlighted
            )
        }
        Err(_) => {
            // Highlighting failed — fall back to escaped plain text
            format!("<pre><code>{}</code></pre>", html_escape(code))
        }
    }
}

/// Render a delimiter block as a horizontal rule.
fn render_delimiter() -> String {
    "<hr>".to_string()
}

/// Render an embed block (Epic 24.7).
///
/// Whitelisted sources (YouTube, Vimeo) are rendered as responsive iframes.
/// All other URLs are rendered as safe anchor links.
/// Data: `{ "service": "...", "source": "...", "embed": "...", "caption": "..." }`
fn render_embed(data: &Value) -> String {
    let embed_url = data
        .get("embed")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("source").and_then(|v| v.as_str()))
        .unwrap_or("");

    if embed_url.is_empty() {
        return String::new();
    }

    let caption = data.get("caption").and_then(|v| v.as_str()).unwrap_or("");

    if is_whitelisted_embed(embed_url) {
        let escaped_url = html_escape(embed_url);
        let mut html = format!(
            "<div class=\"embed-responsive\">\
             <iframe src=\"{escaped_url}\" frameborder=\"0\" allowfullscreen></iframe>\
             </div>"
        );
        if !caption.is_empty() {
            html.push_str(&format!(
                "<p class=\"embed-caption\">{}</p>",
                html_escape(caption)
            ));
        }
        html
    } else if is_safe_url(embed_url) {
        let escaped_url = html_escape(embed_url);
        format!("<a href=\"{escaped_url}\">{escaped_url}</a>")
    } else {
        // Reject non-http(s) URLs (e.g., javascript:) — render as plain text only
        let escaped_url = html_escape(embed_url);
        format!("<span>{escaped_url}</span>")
    }
}

// ---------------------------------------------------------------------------
// Embed whitelist (24.7)
// ---------------------------------------------------------------------------

/// Whitelisted embed URL patterns.
const EMBED_WHITELIST: &[&str] = &[
    "youtube.com/watch",
    "youtube.com/embed/",
    "youtu.be/",
    "vimeo.com/",
    "player.vimeo.com/",
];

/// Check whether the given URL matches one of the whitelisted embed patterns.
fn is_whitelisted_embed(url: &str) -> bool {
    // Normalise: strip protocol prefix for matching
    let normalised = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.");

    EMBED_WHITELIST
        .iter()
        .any(|pattern| normalised.starts_with(pattern))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_paragraph_block() {
        let blocks = vec![json!({
            "type": "paragraph",
            "data": { "text": "Hello, world!" }
        })];
        let html = render_blocks(&blocks);
        assert_eq!(html, "<p>Hello, world!</p>");
    }

    #[test]
    fn render_paragraph_with_inline_html() {
        let blocks = vec![json!({
            "type": "paragraph",
            "data": { "text": "This is <b>bold</b> and <i>italic</i>." }
        })];
        let html = render_blocks(&blocks);
        // ammonia preserves safe inline tags like <b> and <i>
        assert_eq!(html, "<p>This is <b>bold</b> and <i>italic</i>.</p>");
    }

    #[test]
    fn render_paragraph_strips_script_tags() {
        let blocks = vec![json!({
            "type": "paragraph",
            "data": { "text": "Hello <script>alert('xss')</script> world" }
        })];
        let html = render_blocks(&blocks);
        assert!(!html.contains("<script>"), "script tags must be stripped");
        assert!(html.contains("Hello"));
        assert!(html.contains("world"));
    }

    #[test]
    fn render_heading_block_level_3() {
        let blocks = vec![json!({
            "type": "heading",
            "data": { "text": "Section Title", "level": 3 }
        })];
        let html = render_blocks(&blocks);
        assert_eq!(html, "<h3>Section Title</h3>");
    }

    #[test]
    fn render_heading_block_default_level() {
        let blocks = vec![json!({
            "type": "heading",
            "data": { "text": "Default" }
        })];
        let html = render_blocks(&blocks);
        assert_eq!(html, "<h2>Default</h2>");
    }

    #[test]
    fn render_heading_clamps_out_of_range_level() {
        let blocks = vec![json!({
            "type": "heading",
            "data": { "text": "Too high", "level": 9 }
        })];
        let html = render_blocks(&blocks);
        assert_eq!(html, "<h6>Too high</h6>");
    }

    #[test]
    fn render_image_block() {
        let blocks = vec![json!({
            "type": "image",
            "data": {
                "file": { "url": "https://example.com/photo.jpg" },
                "caption": "A nice photo"
            }
        })];
        let html = render_blocks(&blocks);
        assert!(html.contains("<figure>"));
        assert!(html.contains("<img src=\"https://example.com/photo.jpg\""));
        assert!(html.contains("alt=\"A nice photo\""));
        assert!(html.contains("<figcaption>A nice photo</figcaption>"));
        assert!(html.contains("</figure>"));
    }

    #[test]
    fn render_image_block_with_direct_url() {
        let blocks = vec![json!({
            "type": "image",
            "data": {
                "url": "https://example.com/direct.png",
                "caption": "Direct URL"
            }
        })];
        let html = render_blocks(&blocks);
        assert!(html.contains("src=\"https://example.com/direct.png\""));
    }

    #[test]
    fn render_list_ordered() {
        let blocks = vec![json!({
            "type": "list",
            "data": {
                "style": "ordered",
                "items": ["First", "Second", "Third"]
            }
        })];
        let html = render_blocks(&blocks);
        assert!(html.starts_with("<ol>"));
        assert!(html.ends_with("</ol>"));
        assert!(html.contains("<li>First</li>"));
        assert!(html.contains("<li>Second</li>"));
        assert!(html.contains("<li>Third</li>"));
    }

    #[test]
    fn render_list_unordered() {
        let blocks = vec![json!({
            "type": "list",
            "data": {
                "style": "unordered",
                "items": ["Apple", "Banana"]
            }
        })];
        let html = render_blocks(&blocks);
        assert!(html.starts_with("<ul>"));
        assert!(html.ends_with("</ul>"));
        assert!(html.contains("<li>Apple</li>"));
    }

    #[test]
    fn render_list_default_unordered() {
        let blocks = vec![json!({
            "type": "list",
            "data": {
                "items": ["Item"]
            }
        })];
        let html = render_blocks(&blocks);
        assert!(html.starts_with("<ul>"));
    }

    #[test]
    fn render_quote_block() {
        let blocks = vec![json!({
            "type": "quote",
            "data": {
                "text": "To be or not to be.",
                "caption": "Shakespeare"
            }
        })];
        let html = render_blocks(&blocks);
        assert!(html.contains("<blockquote>"));
        assert!(html.contains("<p>To be or not to be.</p>"));
        assert!(html.contains("<cite>Shakespeare</cite>"));
        assert!(html.contains("</blockquote>"));
    }

    #[test]
    fn render_quote_without_caption() {
        let blocks = vec![json!({
            "type": "quote",
            "data": { "text": "Just a quote." }
        })];
        let html = render_blocks(&blocks);
        assert!(html.contains("<blockquote><p>Just a quote.</p></blockquote>"));
        assert!(!html.contains("<cite>"));
    }

    #[test]
    fn render_code_block_with_rust_highlighting() {
        let blocks = vec![json!({
            "type": "code",
            "data": {
                "code": "fn main() {\n    println!(\"hello\");\n}",
                "language": "rust"
            }
        })];
        let html = render_blocks(&blocks);
        assert!(
            html.contains("<pre><code class=\"language-rust\">"),
            "Expected language class in output, got: {html}"
        );
        // syntect produces <span> tags for highlighted tokens
        assert!(
            html.contains("<span"),
            "Expected highlighted spans in output, got: {html}"
        );
    }

    #[test]
    fn render_code_block_unknown_language_fallback() {
        let blocks = vec![json!({
            "type": "code",
            "data": {
                "code": "some code here",
                "language": "nonexistent_language_xyz"
            }
        })];
        let html = render_blocks(&blocks);
        assert!(
            html.contains("<pre><code>some code here</code></pre>"),
            "Expected plain fallback, got: {html}"
        );
    }

    #[test]
    fn render_code_block_no_language() {
        let blocks = vec![json!({
            "type": "code",
            "data": { "code": "plain text code" }
        })];
        let html = render_blocks(&blocks);
        assert!(
            html.contains("<pre><code>plain text code</code></pre>"),
            "Expected plain code block, got: {html}"
        );
    }

    #[test]
    fn render_code_escapes_html_in_plain_fallback() {
        let blocks = vec![json!({
            "type": "code",
            "data": { "code": "<script>alert('xss')</script>" }
        })];
        let html = render_blocks(&blocks);
        assert!(!html.contains("<script>"), "HTML should be escaped");
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn render_delimiter_block() {
        let blocks = vec![json!({
            "type": "delimiter",
            "data": {}
        })];
        let html = render_blocks(&blocks);
        assert_eq!(html, "<hr>");
    }

    #[test]
    fn render_embed_youtube_watch() {
        let blocks = vec![json!({
            "type": "embed",
            "data": {
                "embed": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
                "caption": "A video"
            }
        })];
        let html = render_blocks(&blocks);
        assert!(
            html.contains("<iframe"),
            "YouTube embeds should produce an iframe, got: {html}"
        );
        assert!(html.contains("youtube.com/watch"));
        assert!(html.contains("allowfullscreen"));
    }

    #[test]
    fn render_embed_youtube_short_url() {
        let blocks = vec![json!({
            "type": "embed",
            "data": {
                "embed": "https://youtu.be/dQw4w9WgXcQ"
            }
        })];
        let html = render_blocks(&blocks);
        assert!(
            html.contains("<iframe"),
            "Short YouTube URL should produce iframe"
        );
    }

    #[test]
    fn render_embed_vimeo() {
        let blocks = vec![json!({
            "type": "embed",
            "data": {
                "embed": "https://vimeo.com/123456789"
            }
        })];
        let html = render_blocks(&blocks);
        assert!(
            html.contains("<iframe"),
            "Vimeo embeds should produce an iframe"
        );
        assert!(html.contains("vimeo.com/123456789"));
    }

    #[test]
    fn render_embed_non_whitelisted_url() {
        let blocks = vec![json!({
            "type": "embed",
            "data": {
                "embed": "https://evil.example.com/payload"
            }
        })];
        let html = render_blocks(&blocks);
        assert!(
            !html.contains("<iframe"),
            "Non-whitelisted URLs must not produce an iframe, got: {html}"
        );
        assert!(
            html.contains("<a href="),
            "Non-whitelisted URLs should produce a safe link, got: {html}"
        );
        assert!(html.contains("evil.example.com/payload"));
    }

    #[test]
    fn render_embed_empty_url() {
        let blocks = vec![json!({
            "type": "embed",
            "data": {}
        })];
        let html = render_blocks(&blocks);
        assert!(html.is_empty());
    }

    #[test]
    fn render_multiple_blocks() {
        let blocks = vec![
            json!({ "type": "heading", "data": { "text": "Title", "level": 1 } }),
            json!({ "type": "paragraph", "data": { "text": "Body text." } }),
            json!({ "type": "delimiter", "data": {} }),
        ];
        let html = render_blocks(&blocks);
        assert_eq!(html, "<h1>Title</h1><p>Body text.</p><hr>");
    }

    #[test]
    fn render_unknown_block_type_skipped() {
        let blocks = vec![json!({
            "type": "unknown_widget",
            "data": { "foo": "bar" }
        })];
        let html = render_blocks(&blocks);
        assert!(html.is_empty(), "Unknown types should be silently skipped");
    }

    #[test]
    fn image_escapes_url_and_caption() {
        let blocks = vec![json!({
            "type": "image",
            "data": {
                "url": "https://example.com/photo.jpg?a=1&b=2",
                "caption": "A <b>bold</b> caption"
            }
        })];
        let html = render_blocks(&blocks);
        assert!(
            html.contains("&amp;b=2"),
            "URL ampersands should be escaped"
        );
        assert!(
            html.contains("&lt;b&gt;bold&lt;/b&gt;"),
            "Caption HTML should be escaped"
        );
    }

    #[test]
    fn embed_whitelist_rejects_similar_domains() {
        // Domains that look similar but are not in the whitelist
        assert!(!is_whitelisted_embed("https://notyoutube.com/watch?v=abc"));
        assert!(!is_whitelisted_embed(
            "https://youtube.com.evil.com/watch?v=abc"
        ));
        assert!(!is_whitelisted_embed("https://fakevimeo.com/12345"));
    }

    #[test]
    fn embed_whitelist_accepts_known_sources() {
        assert!(is_whitelisted_embed("https://youtube.com/watch?v=abc123"));
        assert!(is_whitelisted_embed(
            "https://www.youtube.com/watch?v=abc123"
        ));
        assert!(is_whitelisted_embed("https://youtu.be/abc123"));
        assert!(is_whitelisted_embed("https://vimeo.com/123456"));
        assert!(is_whitelisted_embed("https://www.vimeo.com/123456"));
        assert!(is_whitelisted_embed("https://player.vimeo.com/video/123"));
        assert!(is_whitelisted_embed("https://youtube.com/embed/abc123"));
    }

    #[test]
    fn render_embed_javascript_uri_rejected() {
        let blocks = vec![json!({
            "type": "embed",
            "data": { "embed": "javascript:alert('xss')" }
        })];
        let html = render_blocks(&blocks);
        assert!(
            !html.contains("href"),
            "javascript: URIs must not produce href links, got: {html}"
        );
        assert!(html.contains("<span>"));
    }

    #[test]
    fn html_escape_special_chars() {
        assert_eq!(html_escape("<>&\"'"), "&lt;&gt;&amp;&quot;&#x27;");
    }
}
