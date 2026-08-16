//! Render tree consumer - converts RenderElement JSON to HTML via Tera.

use anyhow::{Context, Result};
use serde_json::Value;
use tera::{Context as TeraContext, Tera};

use crate::content::FilterPipeline;
use crate::routes::helpers::html_escape;
use trovato_sdk::render::RenderElement;

/// Consumer that converts RenderElement trees to HTML.
pub struct RenderTreeConsumer {
    _private: (),
}

impl RenderTreeConsumer {
    /// Create a new render tree consumer.
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Render a RenderElement tree to HTML.
    pub fn render(
        &self,
        tera: &Tera,
        element: &RenderElement,
        context: &mut TeraContext,
    ) -> Result<String> {
        self.render_element(tera, element, context)
    }

    /// Render a single element and its children.
    fn render_element(
        &self,
        tera: &Tera,
        element: &RenderElement,
        context: &mut TeraContext,
    ) -> Result<String> {
        // Sort and render children first
        let children_html = self.render_children(tera, element, context)?;

        // Sanitize plugin-supplied `tag` and `attributes` BEFORE they reach the
        // Tera element-template path (XSS-1/XSS-2). Tera precedence otherwise
        // bypasses the SAFE_TAGS / attribute-key / URL-scheme guarantees that
        // only the Rust `render_inline` fallback enforced: `markup.html` emits
        // `<{{ element.tag }}>` and both element templates iterate `attributes`
        // and read `attributes.href` verbatim. Feeding the context a
        // tag-clamped element and a filtered attribute set makes the template
        // path inherit the same guarantees.
        let safe_attrs = element.attributes.as_ref().map(Self::sanitize_attributes);

        // Build element context
        let mut el_context = context.clone();
        let ctx_element = Self::element_for_context(element, &safe_attrs);
        el_context.insert("element", &ctx_element);
        el_context.insert("children", &children_html);

        // Add processed value if present
        if let Some(value) = &element.value {
            let processed = self.process_value(value, element.format.as_deref());
            el_context.insert("value", &processed);
        }

        // Add attributes as individual values for easy access
        if let Some(attrs) = &safe_attrs {
            el_context.insert("attributes", attrs);

            // Extract class list for convenience
            if let Some(classes) = attrs.get("class") {
                let class_str = self.classes_to_string(classes);
                el_context.insert("class", &class_str);
            }
        }

        // Determine template based on element type
        let template_name = self.template_for_type(&element.element_type);

        // Try to render with template, fall back to inline rendering
        if tera.get_template(&template_name).is_ok() {
            tera.render(&template_name, &el_context)
                .with_context(|| format!("failed to render element type: {}", element.element_type))
        } else {
            // Inline fallback rendering
            self.render_inline(element, &children_html)
        }
    }

    /// Render element children, sorted by weight.
    fn render_children(
        &self,
        tera: &Tera,
        element: &RenderElement,
        context: &mut TeraContext,
    ) -> Result<String> {
        use std::fmt::Write;

        if element.children.is_empty() {
            return Ok(String::new());
        }

        // Collect and sort children by weight
        let mut children: Vec<_> = element.children.iter().collect();
        children.sort_by_key(|(_, child)| child.weight.unwrap_or(0));

        let mut html = String::new();
        for (_key, child) in children {
            let child_html = self.render_element(tera, child, context)?;
            // Infallible: write!() to String is infallible
            #[allow(clippy::unwrap_used)]
            write!(html, "{child_html}").unwrap();
        }

        Ok(html)
    }

    /// Process a value through the appropriate filter pipeline.
    ///
    /// Uses `for_format_safe()` to reject unsafe formats like `full_html`
    /// that plugins might request.
    fn process_value(&self, value: &str, format: Option<&str>) -> String {
        FilterPipeline::for_format_safe(format.unwrap_or("plain_text")).process(value)
    }

    /// Convert a classes value (array or string) to a safe, space-separated string.
    ///
    /// Each class value is escaped to prevent attribute injection from
    /// plugin-supplied `RenderElement` data.
    fn classes_to_string(&self, classes: &Value) -> String {
        match classes {
            Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str())
                .map(html_escape)
                .collect::<Vec<_>>()
                .join(" "),
            Value::String(s) => html_escape(s),
            _ => String::new(),
        }
    }

    /// Get the template name for an element type.
    fn template_for_type(&self, element_type: &str) -> String {
        format!("elements/{element_type}.html")
    }

    /// Render an element inline when no template is available.
    ///
    /// Uses semantic HTML tags based on element type: `<article>` for
    /// item-type elements, `<section>` for containers with headings,
    /// `<nav>` for navigation, `<div>` for generic containers.
    fn render_inline(&self, element: &RenderElement, children: &str) -> Result<String> {
        match element.element_type.as_str() {
            "container" => self.render_container(element, children),
            "markup" => self.render_markup(element),
            _ => {
                // Use semantic tag based on element type
                let tag = Self::semantic_tag_for_type(&element.element_type);
                let class = self.get_class_string(element);
                let safe_type = html_escape(&element.element_type);
                let attrs = self.get_extra_attrs(element);
                Ok(format!(
                    "<{tag} class=\"element element--{safe_type}{class_suffix}\"{attrs}>{children}</{tag}>",
                    class_suffix = if class.is_empty() {
                        String::new()
                    } else {
                        format!(" {class}")
                    },
                ))
            }
        }
    }

    /// Map element type names to semantic HTML tags for the fallback renderer.
    ///
    /// Item-type elements use `<article>`, navigation uses `<nav>`,
    /// and unknown types fall back to `<div>`.
    fn semantic_tag_for_type(element_type: &str) -> &'static str {
        if element_type.starts_with("item") {
            "article"
        } else if element_type == "navigation" || element_type == "nav" {
            "nav"
        } else if element_type == "section" {
            "section"
        } else {
            "div"
        }
    }

    /// Render a container element.
    ///
    /// Uses `<section>` when the container has a heading child,
    /// `<div>` otherwise.
    fn render_container(&self, element: &RenderElement, children: &str) -> Result<String> {
        let class = self.get_class_string(element);
        let attrs = self.get_extra_attrs(element);

        // Use <section> when the container has heading content
        let has_heading = element.children.values().any(|child| {
            child
                .tag
                .as_deref()
                .is_some_and(|t| t.starts_with('h') && t.len() == 2)
        });
        let tag = if has_heading { "section" } else { "div" };

        Ok(format!(
            "<{tag} class=\"container{}\"{}>{}</{tag}>",
            if class.is_empty() {
                String::new()
            } else {
                format!(" {class}")
            },
            attrs,
            children
        ))
    }

    /// Safe HTML tags allowed in markup elements.
    ///
    /// Plugins specify tag names via `RenderElement.tag`. Only tags in this
    /// allowlist are permitted; unknown tags fall back to `span`.
    ///
    /// Excluded dangerous tags:
    /// - `input`: clickjacking via hidden fields
    /// - `link`: CSS injection via external stylesheets
    /// - `meta`: open redirects via `http-equiv="refresh"`
    /// - `script`, `iframe`, `object`, `embed`, `form`, `style`: obvious XSS/phishing
    const SAFE_TAGS: &[&str] = &[
        "a",
        "abbr",
        "address",
        "article",
        "aside",
        "b",
        "bdi",
        "bdo",
        "blockquote",
        "br",
        "caption",
        "cite",
        "code",
        "col",
        "colgroup",
        "dd",
        "del",
        "details",
        "dfn",
        "div",
        "dl",
        "dt",
        "em",
        "figcaption",
        "figure",
        "footer",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "header",
        "hr",
        "i",
        "img",
        "ins",
        "kbd",
        "li",
        "main",
        "mark",
        "nav",
        "ol",
        "p",
        "pre",
        "q",
        "rp",
        "rt",
        "ruby",
        "s",
        "samp",
        "section",
        "small",
        "span",
        "strong",
        "sub",
        "summary",
        "sup",
        "table",
        "tbody",
        "td",
        "tfoot",
        "th",
        "thead",
        "time",
        "tr",
        "u",
        "ul",
        "var",
        "wbr",
    ];

    /// Clamp a plugin-supplied tag name to the [`SAFE_TAGS`](Self::SAFE_TAGS)
    /// allowlist, falling back to `span` for anything not on it.
    fn clamp_tag(requested_tag: &str) -> &str {
        if Self::SAFE_TAGS.contains(&requested_tag) {
            requested_tag
        } else {
            "span"
        }
    }

    /// Render a markup element.
    fn render_markup(&self, element: &RenderElement) -> Result<String> {
        let requested_tag = element.tag.as_deref().unwrap_or("span");
        let tag = Self::clamp_tag(requested_tag);
        let value = element
            .value
            .as_ref()
            .map(|v| self.process_value(v, element.format.as_deref()))
            .unwrap_or_default();

        let class = self.get_class_string(element);
        let attrs = self.get_extra_attrs(element);

        // Void elements (no closing tag)
        let void_elements = ["br", "hr", "img", "col", "wbr"];
        if void_elements.contains(&tag) {
            return Ok(format!(
                "<{}{}{} />",
                tag,
                if class.is_empty() {
                    String::new()
                } else {
                    format!(" class=\"{class}\"")
                },
                attrs
            ));
        }

        Ok(format!(
            "<{}{}{}>{}</{}>",
            tag,
            if class.is_empty() {
                String::new()
            } else {
                format!(" class=\"{class}\"")
            },
            attrs,
            value,
            tag
        ))
    }

    /// Get class string from element attributes.
    fn get_class_string(&self, element: &RenderElement) -> String {
        element
            .attributes
            .as_ref()
            .and_then(|attrs| attrs.get("class"))
            .map(|classes| self.classes_to_string(classes))
            .unwrap_or_default()
    }

    /// Get extra attributes (excluding class) as a string.
    ///
    /// Attribute keys are validated to contain only safe characters
    /// (`[a-zA-Z0-9-_]`) to prevent attribute injection from plugin-sourced
    /// `RenderElement` data. Keys failing validation are silently skipped.
    fn get_extra_attrs(&self, element: &RenderElement) -> String {
        let Some(attrs) = &element.attributes else {
            return String::new();
        };

        let Value::Object(obj) = attrs else {
            return String::new();
        };

        obj.iter()
            .filter(|(k, _)| *k != "class")
            .filter(|(k, _)| Self::is_valid_attr_key(k))
            .filter_map(|(k, v)| {
                // XSS-2: drop URL-bearing attributes whose scheme is not
                // allowlisted (`javascript:`, `data:`, …).
                if (k == "href" || k == "src")
                    && let Value::String(s) = v
                    && !Self::is_safe_url(s)
                {
                    return None;
                }
                let value = match v {
                    Value::String(s) => html_escape(s),
                    Value::Bool(b) => {
                        if *b {
                            return Some(format!(" {k}"));
                        } else {
                            return Some(String::new());
                        }
                    }
                    _ => html_escape(&v.to_string()),
                };
                Some(format!(" {k}=\"{value}\""))
            })
            .collect()
    }

    /// Build a tag-clamped, attribute-sanitized shallow copy of `element` for
    /// insertion into the Tera element context.
    ///
    /// Children are intentionally omitted: element templates consume the
    /// pre-rendered `children` HTML string, never `element.children`, so an
    /// empty child map avoids a deep clone while `markup.html`'s
    /// `{{ element.tag }}` resolves to a [`SAFE_TAGS`](Self::SAFE_TAGS) value.
    fn element_for_context(element: &RenderElement, safe_attrs: &Option<Value>) -> RenderElement {
        RenderElement {
            element_type: element.element_type.clone(),
            weight: element.weight,
            tag: element
                .tag
                .as_deref()
                .map(|t| Self::clamp_tag(t).to_string()),
            value: element.value.clone(),
            format: element.format.clone(),
            attributes: safe_attrs.clone(),
            children: std::collections::BTreeMap::new(),
        }
    }

    /// Filter a plugin-supplied attribute object: drop keys that fail
    /// [`is_valid_attr_key`](Self::is_valid_attr_key) and URL attributes
    /// (`href`/`src`) whose scheme is not allowlisted. `class` is preserved
    /// verbatim (rendered separately). Returns a `Value::Object`.
    fn sanitize_attributes(attrs: &Value) -> Value {
        let Value::Object(obj) = attrs else {
            return Value::Object(serde_json::Map::new());
        };
        let mut out = serde_json::Map::new();
        for (k, v) in obj {
            if k == "class" {
                out.insert(k.clone(), v.clone());
                continue;
            }
            if !Self::is_valid_attr_key(k) {
                continue;
            }
            if (k == "href" || k == "src")
                && let Value::String(s) = v
                && !Self::is_safe_url(s)
            {
                continue;
            }
            out.insert(k.clone(), v.clone());
        }
        Value::Object(out)
    }

    /// Check if an attribute key is safe to render.
    ///
    /// Valid keys match `[a-zA-Z][a-zA-Z0-9-_]*` — must start with a letter and
    /// contain only alphanumerics, hyphens, and underscores — AND must not be an
    /// `on*` event-handler attribute (`onerror`, `onmouseover`, …), which would
    /// execute script even with an HTML-escaped value (XSS-1).
    fn is_valid_attr_key(key: &str) -> bool {
        // Reject event-handler attributes outright.
        if key.len() >= 2 && key[..2].eq_ignore_ascii_case("on") {
            return false;
        }
        let mut chars = key.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphabetic() => {}
            _ => return false,
        }
        chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }

    /// Whether a `href`/`src` URL carries a safe scheme.
    ///
    /// Relative URLs (no scheme) and the `http`/`https`/`mailto`/`tel`/`ftp`
    /// schemes are allowed; everything else (`javascript:`, `data:`,
    /// `vbscript:`, …) is rejected. Whitespace and control characters are
    /// stripped first so `java\tscript:` can't smuggle a scheme past the check
    /// (browsers ignore those when parsing the scheme).
    fn is_safe_url(value: &str) -> bool {
        let cleaned: String = value
            .chars()
            .filter(|c| !c.is_whitespace() && !c.is_control())
            .collect();
        let lower = cleaned.to_ascii_lowercase();
        // A scheme exists only if a ':' precedes any path/query/fragment start.
        if let Some(idx) = lower.find([':', '/', '?', '#'])
            && lower.as_bytes()[idx] == b':'
        {
            let scheme = &lower[..idx];
            return matches!(scheme, "http" | "https" | "mailto" | "tel" | "ftp");
        }
        // No scheme ⇒ relative URL ⇒ safe.
        true
    }
}

impl Default for RenderTreeConsumer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Tests marked `SECURITY REGRESSION TEST` verify fixes for specific security
    //! findings from Epic 27. Do not remove without security review.

    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn test_render_markup_basic() {
        let consumer = RenderTreeConsumer::new();

        let element = RenderElement {
            element_type: "markup".to_string(),
            weight: None,
            tag: Some("p".to_string()),
            value: Some("Hello world".to_string()),
            format: Some("plain_text".to_string()),
            attributes: None,
            children: BTreeMap::new(),
        };

        let result = consumer.render_markup(&element).unwrap();
        assert_eq!(result, "<p>Hello world</p>");
    }

    #[test]
    fn test_render_markup_with_class() {
        let consumer = RenderTreeConsumer::new();

        let mut attrs = serde_json::Map::new();
        attrs.insert(
            "class".to_string(),
            Value::Array(vec![Value::String("text".to_string())]),
        );

        let element = RenderElement {
            element_type: "markup".to_string(),
            weight: None,
            tag: Some("span".to_string()),
            value: Some("Test".to_string()),
            format: None,
            attributes: Some(Value::Object(attrs)),
            children: BTreeMap::new(),
        };

        let result = consumer.render_markup(&element).unwrap();
        assert!(result.contains("class=\"text\""));
    }

    #[test]
    fn test_render_container() {
        let consumer = RenderTreeConsumer::new();

        let element = RenderElement {
            element_type: "container".to_string(),
            weight: None,
            tag: None,
            value: None,
            format: None,
            attributes: None,
            children: BTreeMap::new(),
        };

        let result = consumer.render_container(&element, "<p>Child</p>").unwrap();
        assert!(result.contains("container"));
        assert!(result.contains("<p>Child</p>"));
    }

    #[test]
    fn test_process_value_plain_text() {
        let consumer = RenderTreeConsumer::new();
        let result = consumer.process_value("<script>alert('xss')</script>", Some("plain_text"));
        assert!(!result.contains("<script>"));
        assert!(result.contains("&lt;script&gt;"));
    }

    #[test]
    fn test_classes_to_string_array() {
        let consumer = RenderTreeConsumer::new();
        let classes = Value::Array(vec![
            Value::String("foo".to_string()),
            Value::String("bar".to_string()),
        ]);
        assert_eq!(consumer.classes_to_string(&classes), "foo bar");
    }

    #[test]
    fn test_classes_to_string_string() {
        let consumer = RenderTreeConsumer::new();
        let classes = Value::String("foo bar".to_string());
        assert_eq!(consumer.classes_to_string(&classes), "foo bar");
    }

    #[test]
    fn test_html_escape() {
        assert_eq!(html_escape("<>&\"'"), "&lt;&gt;&amp;&quot;&#x27;");
    }

    // SECURITY REGRESSION TEST — Story 27.1 Finding B: attr key validation
    #[test]
    fn test_valid_attr_keys() {
        assert!(RenderTreeConsumer::is_valid_attr_key("id"));
        assert!(RenderTreeConsumer::is_valid_attr_key("data-id"));
        assert!(RenderTreeConsumer::is_valid_attr_key("aria-label"));
        assert!(RenderTreeConsumer::is_valid_attr_key("data_custom"));
        assert!(RenderTreeConsumer::is_valid_attr_key("X"));
    }

    // SECURITY REGRESSION TEST — Story 27.1 Finding B: invalid attr keys rejected
    #[test]
    fn test_invalid_attr_keys() {
        assert!(!RenderTreeConsumer::is_valid_attr_key(""));
        assert!(!RenderTreeConsumer::is_valid_attr_key("1data"));
        assert!(!RenderTreeConsumer::is_valid_attr_key("on\"click"));
        assert!(!RenderTreeConsumer::is_valid_attr_key("x>y"));
        assert!(!RenderTreeConsumer::is_valid_attr_key("a b"));
        assert!(!RenderTreeConsumer::is_valid_attr_key("-start"));
    }

    // SECURITY REGRESSION TEST — Story 27.1 Finding B: invalid keys silently skipped
    #[test]
    fn test_get_extra_attrs_skips_invalid_keys() {
        let consumer = RenderTreeConsumer::new();

        let mut attrs = serde_json::Map::new();
        attrs.insert("data-id".to_string(), Value::String("safe".to_string()));
        attrs.insert("on\"click".to_string(), Value::String("evil".to_string()));

        let element = RenderElement {
            element_type: "markup".to_string(),
            weight: None,
            tag: Some("div".to_string()),
            value: None,
            format: None,
            attributes: Some(Value::Object(attrs)),
            children: BTreeMap::new(),
        };

        let result = consumer.get_extra_attrs(&element);
        assert!(result.contains("data-id=\"safe\""));
        assert!(!result.contains("evil"));
    }

    // SECURITY REGRESSION TEST — Story 27.1 Finding R2-3/R2-4: unsafe tags fall back to <span>
    #[test]
    fn test_render_markup_rejects_unsafe_tag() {
        let consumer = RenderTreeConsumer::new();

        for tag in &[
            "script", "iframe", "object", "embed", "form", "style", "input", "link", "meta",
        ] {
            let element = RenderElement {
                element_type: "markup".to_string(),
                weight: None,
                tag: Some(tag.to_string()),
                value: Some("test".to_string()),
                format: Some("plain_text".to_string()),
                attributes: None,
                children: BTreeMap::new(),
            };
            let result = consumer.render_markup(&element).unwrap();
            assert!(
                result.starts_with("<span"),
                "unsafe tag '{tag}' should fall back to <span>, got: {result}"
            );
        }
    }

    // SECURITY REGRESSION TEST — Story 27.1 Finding R2-3: safe tags allowed
    #[test]
    fn test_render_markup_allows_safe_tags() {
        let consumer = RenderTreeConsumer::new();

        for tag in &[
            "p", "div", "span", "h1", "strong", "em", "a", "ul", "li", "table",
        ] {
            let element = RenderElement {
                element_type: "markup".to_string(),
                weight: None,
                tag: Some(tag.to_string()),
                value: Some("content".to_string()),
                format: Some("plain_text".to_string()),
                attributes: None,
                children: BTreeMap::new(),
            };
            let result = consumer.render_markup(&element).unwrap();
            assert!(
                result.starts_with(&format!("<{tag}")),
                "safe tag '{tag}' should be allowed, got: {result}"
            );
        }
    }

    // SECURITY REGRESSION TEST — Story 27.1 Finding R2-2: class attribute injection prevented
    #[test]
    fn test_classes_to_string_escapes_quotes() {
        let consumer = RenderTreeConsumer::new();
        let classes = Value::Array(vec![
            Value::String("safe".to_string()),
            Value::String("x\" onload=\"alert(1)".to_string()),
        ]);
        let result = consumer.classes_to_string(&classes);
        assert!(!result.contains('"'));
        assert!(result.contains("&quot;"));
    }

    // SECURITY REGRESSION TEST — Story 27.1 Finding H1: process_value whitelists format
    #[test]
    fn test_process_value_rejects_full_html() {
        let consumer = RenderTreeConsumer::new();
        let result = consumer.process_value("<script>alert('xss')</script>", Some("full_html"));
        assert!(!result.contains("<script>"));
        assert!(result.contains("&lt;script&gt;"));
    }

    // ---------------------------------------------------------------------
    // FR-6 audit: XSS-1 (plugin tag/attr bypass via the Tera path) and XSS-2
    // (javascript:/data: URL schemes in href/src).
    // ---------------------------------------------------------------------

    // SECURITY REGRESSION TEST — XSS-2: URL scheme allowlist for href/src.
    #[test]
    fn test_is_safe_url_scheme_allowlist() {
        // Allowed: relative, anchor, http(s), mailto, tel.
        for ok in [
            "/foo/bar",
            "images/x.png",
            "#section",
            "?q=1",
            "http://example.com",
            "https://example.com/a?b=c",
            "HTTPS://EXAMPLE.COM",
            "mailto:a@b.com",
            "tel:+123",
        ] {
            assert!(RenderTreeConsumer::is_safe_url(ok), "should allow {ok}");
        }
        // Rejected: script/data/other dangerous schemes, incl. whitespace and
        // case smuggling that browsers would still execute.
        for bad in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "  javascript:alert(1)",
            "java\tscript:alert(1)",
            "java\nscript:alert(1)",
            "data:text/html;base64,PHNjcmlwdD4=",
            "vbscript:msgbox(1)",
        ] {
            assert!(
                !RenderTreeConsumer::is_safe_url(bad),
                "should reject {bad:?}"
            );
        }
    }

    // SECURITY REGRESSION TEST — XSS-1: event-handler attribute keys rejected.
    #[test]
    fn test_is_valid_attr_key_rejects_event_handlers() {
        for bad in ["onerror", "onmouseover", "ONCLICK", "onLoad", "on"] {
            assert!(
                !RenderTreeConsumer::is_valid_attr_key(bad),
                "should reject event-handler key {bad}"
            );
        }
        // Non-event keys that merely start with other letters are fine.
        assert!(RenderTreeConsumer::is_valid_attr_key("open"));
        assert!(RenderTreeConsumer::is_valid_attr_key("title"));
    }

    // SECURITY REGRESSION TEST — XSS-1/XSS-2: sanitize_attributes drops
    // event-handler keys and unsafe URL schemes, keeps class + safe attrs.
    #[test]
    fn test_sanitize_attributes_drops_dangerous() {
        let mut attrs = serde_json::Map::new();
        attrs.insert("title".to_string(), Value::String("ok".to_string()));
        attrs.insert("onerror".to_string(), Value::String("alert(1)".to_string()));
        attrs.insert(
            "href".to_string(),
            Value::String("javascript:alert(1)".to_string()),
        );
        attrs.insert("src".to_string(), Value::String("/safe.png".to_string()));
        attrs.insert(
            "class".to_string(),
            Value::Array(vec![Value::String("c".to_string())]),
        );

        let out = RenderTreeConsumer::sanitize_attributes(&Value::Object(attrs));
        let obj = out.as_object().unwrap();
        assert!(obj.contains_key("title"), "keeps safe attr");
        assert!(obj.contains_key("class"), "keeps class");
        assert!(obj.contains_key("src"), "keeps safe src");
        assert!(!obj.contains_key("onerror"), "drops event handler");
        assert!(!obj.contains_key("href"), "drops javascript: href");
    }

    /// Build a Tera loaded with the real element templates so a test can drive
    /// the template (not fallback) render path.
    fn tera_with_element_templates() -> Tera {
        let mut tera = Tera::default();
        tera.add_raw_template(
            "elements/markup.html",
            include_str!("../../../../templates/elements/markup.html"),
        )
        .unwrap();
        tera.add_raw_template(
            "elements/link.html",
            include_str!("../../../../templates/elements/link.html"),
        )
        .unwrap();
        tera
    }

    // SECURITY REGRESSION TEST — XSS-1: the Tera element-template path (not the
    // Rust fallback) must NOT emit a plugin-chosen dangerous tag or an
    // event-handler attribute. This is the exact bypass the audit flagged.
    #[test]
    fn test_tera_markup_path_sanitizes_tag_and_attrs() {
        let consumer = RenderTreeConsumer::new();
        let tera = tera_with_element_templates();

        let mut attrs = serde_json::Map::new();
        attrs.insert("onerror".to_string(), Value::String("alert(1)".to_string()));
        attrs.insert("title".to_string(), Value::String("ok".to_string()));

        let element = RenderElement {
            element_type: "markup".to_string(),
            weight: None,
            tag: Some("script".to_string()),
            value: Some("hi".to_string()),
            format: Some("plain_text".to_string()),
            attributes: Some(Value::Object(attrs)),
            children: BTreeMap::new(),
        };

        let mut ctx = TeraContext::new();
        let html = consumer.render(&tera, &element, &mut ctx).unwrap();

        assert!(
            !html.contains("<script"),
            "plugin tag must be clamped to span, got: {html}"
        );
        assert!(html.contains("<span"), "expected span, got: {html}");
        assert!(
            !html.contains("onerror"),
            "event-handler attr must be dropped, got: {html}"
        );
        assert!(html.contains("title=\"ok\""), "safe attr kept: {html}");
    }

    // SECURITY REGRESSION TEST — XSS-2: the link template must not emit a
    // `javascript:` href; the sanitizer drops it and the template falls back
    // to "#".
    #[test]
    fn test_tera_link_path_blocks_javascript_href() {
        let consumer = RenderTreeConsumer::new();
        let tera = tera_with_element_templates();

        let mut attrs = serde_json::Map::new();
        attrs.insert(
            "href".to_string(),
            Value::String("javascript:alert(1)".to_string()),
        );

        let element = RenderElement {
            element_type: "link".to_string(),
            weight: None,
            tag: None,
            value: Some("click".to_string()),
            format: Some("plain_text".to_string()),
            attributes: Some(Value::Object(attrs)),
            children: BTreeMap::new(),
        };

        let mut ctx = TeraContext::new();
        let html = consumer.render(&tera, &element, &mut ctx).unwrap();

        assert!(
            !html.contains("javascript:"),
            "javascript: href must be blocked, got: {html}"
        );
        assert!(html.contains("href=\"#\""), "href falls back to #: {html}");
    }
}
