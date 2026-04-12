//! Puck-based page builder renderer.
//!
//! Parses Puck JSON component trees and renders them to HTML using Tera
//! templates. Each component type maps to a `pb/{kebab-name}.html` template.
//! Zone-based nesting allows arbitrary component composition (e.g., Columns
//! containing CTA + TextBlock children).
//!
//! Security: each component is sanitized individually via Ammonia before
//! assembly. The page wrapper template is kernel-controlled and trusted.

use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Regex for extracting heading levels from rendered HTML.
// Infallible: hard-coded valid regex pattern — Regex::new cannot fail here.
#[allow(clippy::expect_used)]
static HEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<h([2-6])[^>]*>").expect("hard-coded regex"));

/// Regex for rewriting Markdown heading levels.
// Infallible: hard-coded valid regex pattern — Regex::new cannot fail here.
#[allow(clippy::expect_used)]
static MD_HEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^(#{1,6})\s").expect("hard-coded regex"));

/// Maximum zone depth to prevent infinite recursion from malformed JSON.
const MAX_RECURSION_DEPTH: usize = 10;

/// Maximum zone index to iterate when collecting rendered zones.
const MAX_ZONE_INDEX: usize = 12;

/// A full Puck page as stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PuckPage {
    /// Page-level metadata.
    pub root: Option<PuckRoot>,
    /// Top-level component list.
    pub content: Vec<PuckComponent>,
}

/// Page-level metadata from Puck's root config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PuckRoot {
    /// Page title (may be used by the page template).
    pub title: Option<String>,
}

/// A single Puck component with optional child zones.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PuckComponent {
    /// PascalCase component type name (e.g., `"Hero"`, `"Columns"`, `"CardGrid"`).
    #[serde(rename = "type")]
    pub component_type: String,
    /// Component-specific properties (arbitrary JSON object).
    #[serde(default)]
    pub props: serde_json::Value,
    /// Named drop zones containing child components.
    #[serde(default)]
    pub zones: HashMap<String, Vec<PuckComponent>>,
}

/// Convert PascalCase to kebab-case for template lookup.
///
/// `"Hero"` → `"hero"`, `"CardGrid"` → `"card-grid"`,
/// `"ContentFeature"` → `"content-feature"`.
///
/// Note: all-caps names like `"CTA"` become `"c-t-a"`. Use `"Cta"` instead.
fn to_kebab_case(name: &str) -> String {
    let mut result = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            result.push('-');
        }
        result.push(ch.to_ascii_lowercase());
    }
    result
}

/// Build the Ammonia sanitizer for page builder components.
///
/// Allows: class, style (for background-image, gap, background-color),
/// semantic HTML tags used by components, and standard formatting.
fn build_sanitizer() -> ammonia::Builder<'static> {
    let mut builder = ammonia::Builder::default();
    // Allow class for component styling (pb-hero, pb-columns, etc.)
    // Allow style for inline CSS (background-image, gap, background-color)
    // TODO: restrict allowed CSS properties to a whitelist for production
    builder.add_generic_attributes(["class", "style"]);
    // Allow semantic tags used by components
    builder.add_tags([
        "section",
        "aside",
        "details",
        "summary",
        "article",
        "figure",
        "figcaption",
        "nav",
        "iframe",
    ]);
    // Allow iframe attributes needed for YouTube embeds
    builder.add_tag_attributes(
        "iframe",
        [
            "src",
            "title",
            "frameborder",
            "allow",
            "allowfullscreen",
            "loading",
        ],
    );
    // Allow loading="lazy" on images
    builder.add_tag_attributes("img", ["loading"]);
    // Allow name on details (for accordion exclusive open)
    builder.add_tag_attributes("details", ["name"]);
    // Allow aria-label and role on any element
    builder.add_generic_attributes(["aria-label", "role", "lang"]);
    builder
}

/// Validate accessibility constraints on a component's props.
///
/// Returns warnings on success, or a fatal error string if the component
/// must not be rendered (e.g., missing alt text on images).
fn validate_accessibility(
    component_type: &str,
    props: &serde_json::Value,
) -> std::result::Result<Vec<String>, String> {
    let mut warnings = Vec::new();

    // Rule 1: Images must have alt text (WCAG 1.1.1)
    let image_props = ["backgroundImage", "imageUrl", "image_url"];
    let alt_props = ["imageAlt", "image_alt", "alt"];

    let has_image = image_props.iter().any(|p| {
        props
            .get(p)
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
    });

    if has_image {
        let has_alt = alt_props.iter().any(|p| {
            props
                .get(p)
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty())
        });
        let is_decorative = props
            .get("isDecorative")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !has_alt && !is_decorative {
            return Err(format!(
                "component '{component_type}' has an image but no alt text \
                 (and isDecorative is not true) — WCAG 1.1.1"
            ));
        }
    }

    // Rule 2: headingLevel must be 2-6 (page template owns H1)
    if let Some(level) = props.get("headingLevel").and_then(|v| v.as_u64())
        && !(2..=6).contains(&level)
    {
        return Err(format!(
            "component '{component_type}' has headingLevel={level} — must be 2-6"
        ));
    }

    // Rule 3: YouTubeEmbed should have a title (WCAG 4.1.2)
    if component_type == "YouTubeEmbed"
        && props
            .get("title")
            .and_then(|v| v.as_str())
            .is_none_or(|s| s.is_empty())
    {
        warnings.push(
            "YouTubeEmbed missing 'title' — screen readers use this to identify the iframe"
                .to_string(),
        );
    }

    Ok(warnings)
}

/// Render a full Puck page to HTML.
///
/// Returns the rendered body HTML (component outputs concatenated).
/// The caller wraps this in the page-level Tera template.
///
/// After rendering, validates heading hierarchy across the full page
/// and logs warnings for skipped heading levels.
pub fn render_puck_page(page: &PuckPage, tera: &tera::Tera) -> Result<String> {
    let sanitizer = build_sanitizer();
    let mut parts = Vec::with_capacity(page.content.len());
    for component in &page.content {
        let html = render_component(component, tera, &sanitizer, 0)?;
        parts.push(html);
    }
    let rendered = parts.join("\n");

    // Post-render heading hierarchy validation (warnings, not errors)
    let hierarchy_warnings = validate_heading_hierarchy(&rendered);
    for w in &hierarchy_warnings {
        tracing::warn!(warning = %w, "page builder heading hierarchy issue");
    }

    Ok(rendered)
}

/// Validate heading hierarchy across the full rendered page.
///
/// Scans rendered HTML for `<h2>`..`<h6>` tags and checks that no heading
/// level is skipped (e.g., H2 → H4 without H3 between them). Returns
/// warnings (not errors) — broken hierarchy is a content quality issue
/// that should surface in monitoring, not block rendering.
fn validate_heading_hierarchy(rendered_html: &str) -> Vec<String> {
    let mut warnings = Vec::new();

    let levels: Vec<u8> = HEADING_RE
        .captures_iter(rendered_html)
        .filter_map(|cap| cap[1].parse().ok())
        .collect();

    if levels.is_empty() {
        return warnings;
    }

    for window in levels.windows(2) {
        let current = window[0];
        let next = window[1];
        // Going deeper: next should be at most current + 1
        if next > current + 1 {
            warnings.push(format!(
                "heading hierarchy skip: H{current} → H{next} (expected H{} or same/higher level)",
                current + 1
            ));
        }
    }

    warnings
}

/// Rewrite Markdown headings so the minimum level is `min_level`.
///
/// `# Foo` with min_level=2 becomes `## Foo`.
/// `## Foo` with min_level=2 stays `## Foo`.
/// `### Foo` with min_level=2 stays `### Foo`.
pub fn rewrite_markdown_headings(text: &str, min_level: usize) -> String {
    MD_HEADING_RE
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let current_level = caps[1].len();
            let new_level = current_level.max(min_level);
            format!("{} ", "#".repeat(new_level))
        })
        .into_owned()
}

/// Render a single Puck component to HTML, recursively rendering zone children.
fn render_component(
    component: &PuckComponent,
    tera: &tera::Tera,
    sanitizer: &ammonia::Builder<'_>,
    depth: usize,
) -> Result<String> {
    if depth > MAX_RECURSION_DEPTH {
        anyhow::bail!(
            "page builder recursion depth exceeded ({MAX_RECURSION_DEPTH}) \
             for component type '{}'",
            component.component_type
        );
    }

    let kebab = to_kebab_case(&component.component_type);
    let template_name = format!("pb/{kebab}.html");

    // Skip unknown component types gracefully
    if tera.get_template(&template_name).is_err() {
        tracing::warn!(
            component_type = %component.component_type,
            template = %template_name,
            "unknown page builder component type, skipping"
        );
        return Ok(String::new());
    }

    // Accessibility validation
    match validate_accessibility(&component.component_type, &component.props) {
        Err(fatal) => {
            tracing::error!(
                component_type = %component.component_type,
                error = %fatal,
                "accessibility validation failed, skipping component"
            );
            return Ok(format!("<!-- a11y error: {fatal} -->"));
        }
        Ok(warnings) => {
            for w in &warnings {
                tracing::warn!(
                    component_type = %component.component_type,
                    warning = %w,
                    "accessibility warning"
                );
            }
        }
    }

    // Build Tera context from props
    let mut context = tera::Context::new();
    if let Some(obj) = component.props.as_object() {
        for (key, value) in obj {
            context.insert(key, value);
        }
    }

    // Recursively render zone children BEFORE rendering parent
    if !component.zones.is_empty() {
        let mut rendered_zones: HashMap<String, Vec<String>> = HashMap::new();

        // Collect all zones by iterating the actual keys
        for (zone_key, children) in &component.zones {
            let zone_html: Vec<String> = children
                .iter()
                .map(|child| render_component(child, tera, sanitizer, depth + 1))
                .collect::<Result<Vec<_>>>()?;
            rendered_zones.insert(zone_key.clone(), zone_html);
        }

        context.insert("zones", &rendered_zones);
    }

    // Render template
    let html = tera
        .render(&template_name, &context)
        .with_context(|| format!("failed to render component '{}'", component.component_type))?;

    // Sanitize per-component
    Ok(sanitizer.clean(&html).to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_tera() -> tera::Tera {
        let mut tera = tera::Tera::default();
        // Register the markdown filter with min_heading support (matches engine.rs)
        tera.register_filter(
            "markdown",
            |value: &tera::Value, args: &std::collections::HashMap<String, tera::Value>| {
                let Some(text) = value.as_str() else {
                    return Ok(tera::Value::String(String::new()));
                };
                let min_heading = args
                    .get("min_heading")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1) as usize;
                let processed = if min_heading > 1 {
                    rewrite_markdown_headings(text, min_heading)
                } else {
                    text.to_string()
                };
                let parser = pulldown_cmark::Parser::new(&processed);
                let mut html_output = String::new();
                pulldown_cmark::html::push_html(&mut html_output, parser);
                Ok(tera::Value::String(ammonia::clean(&html_output)))
            },
        );

        let template_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("templates/pb");

        // Load all pb/*.html templates from the templates directory.
        // Load macros.html first so {% import %} resolves in other templates.
        if template_dir.exists() {
            let macros_path = template_dir.join("macros.html");
            if macros_path.exists() {
                let content = std::fs::read_to_string(&macros_path).unwrap();
                tera.add_raw_template("pb/macros.html", &content).unwrap();
            }
            for entry in std::fs::read_dir(&template_dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "html") {
                    let name = format!("pb/{}", path.file_name().unwrap().to_str().unwrap());
                    if name == "pb/macros.html" {
                        continue; // Already loaded
                    }
                    let content = std::fs::read_to_string(&path).unwrap();
                    tera.add_raw_template(&name, &content).unwrap();
                }
            }
        }

        tera
    }

    #[test]
    fn to_kebab_case_conversions() {
        assert_eq!(to_kebab_case("Hero"), "hero");
        assert_eq!(to_kebab_case("CardGrid"), "card-grid");
        assert_eq!(to_kebab_case("ContentFeature"), "content-feature");
        assert_eq!(to_kebab_case("TextBlock"), "text-block");
        assert_eq!(to_kebab_case("SectionWrapper"), "section-wrapper");
        assert_eq!(to_kebab_case("YouTubeEmbed"), "you-tube-embed");
        // Use "Cta" not "CTA" to get "cta"
        assert_eq!(to_kebab_case("Cta"), "cta");
    }

    #[test]
    fn puck_json_deserializes() {
        let json_str = r#"{
            "root": { "title": "Test Page" },
            "content": [
                { "type": "Hero", "props": { "title": "Hello", "variant": "standard" } },
                { "type": "Columns", "props": { "layout": "1/2+1/2" }, "zones": {
                    "zone-0": [{ "type": "Cta", "props": { "heading": "Act Now" } }],
                    "zone-1": []
                }}
            ]
        }"#;

        let page: PuckPage = serde_json::from_str(json_str).unwrap();
        assert_eq!(page.content.len(), 2);
        assert_eq!(page.content[0].component_type, "Hero");
        assert_eq!(page.content[1].zones.len(), 2);
    }

    #[test]
    fn render_hero_standard() {
        let tera = test_tera();
        let page: PuckPage = serde_json::from_value(json!({
            "content": [{
                "type": "Hero",
                "props": {
                    "title": "Build Better Websites",
                    "subtitle": "Enterprise expertise",
                    "ctaText": "Get Started",
                    "ctaUrl": "/contact",
                    "variant": "standard"
                }
            }]
        }))
        .unwrap();

        let html = render_puck_page(&page, &tera).unwrap();
        assert!(html.contains("pb-hero--standard"), "html: {html}");
        assert!(html.contains("Build Better Websites"));
        assert!(html.contains("/contact"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn render_columns_with_nested_children() {
        let tera = test_tera();
        let page: PuckPage = serde_json::from_value(json!({
            "content": [{
                "type": "Columns",
                "props": { "layout": "2/3+1/3", "gap": "2rem" },
                "zones": {
                    "zone-0": [{
                        "type": "Cta",
                        "props": { "heading": "Left CTA", "buttonText": "Click", "buttonUrl": "/go" }
                    }],
                    "zone-1": [{
                        "type": "Hero",
                        "props": { "title": "Right Hero", "variant": "minimal" }
                    }]
                }
            }]
        }))
        .unwrap();

        let html = render_puck_page(&page, &tera).unwrap();
        assert!(html.contains("pb-columns"), "html: {html}");
        assert!(html.contains("Left CTA"));
        assert!(html.contains("Right Hero"));
    }

    #[test]
    fn render_text_block_with_markdown() {
        let tera = test_tera();
        let page: PuckPage = serde_json::from_value(json!({
            "content": [{
                "type": "TextBlock",
                "props": { "content": "## Hello\n\nThis is **bold**." }
            }]
        }))
        .unwrap();

        let html = render_puck_page(&page, &tera).unwrap();
        assert!(html.contains("pb-text-block"), "html: {html}");
        assert!(html.contains("<h2>"));
        assert!(html.contains("<strong>bold</strong>"));
    }

    #[test]
    fn render_unknown_component_skips() {
        let tera = test_tera();
        let page: PuckPage = serde_json::from_value(json!({
            "content": [{
                "type": "NonexistentWidget",
                "props": { "foo": "bar" }
            }]
        }))
        .unwrap();

        let html = render_puck_page(&page, &tera).unwrap();
        assert!(html.trim().is_empty());
    }

    #[test]
    fn render_xss_in_props_sanitized() {
        let tera = test_tera();
        let page: PuckPage = serde_json::from_value(json!({
            "content": [{
                "type": "Hero",
                "props": {
                    "title": "<script>alert('xss')</script>Legit Title",
                    "variant": "standard"
                }
            }]
        }))
        .unwrap();

        let html = render_puck_page(&page, &tera).unwrap();
        // Ammonia strips <script> elements. The title text in the <h2> must be
        // escaped (not executable). Note: aria-label attribute still contains the
        // raw title string, but attribute values can't execute scripts.
        assert!(
            html.contains("&lt;script&gt;"),
            "script should be HTML-escaped in heading text: {html}"
        );
        assert!(html.contains("Legit Title"));
    }

    #[test]
    fn render_max_recursion_depth_errors() {
        let tera = test_tera();
        let mut nested = json!({
            "type": "Hero",
            "props": { "title": "deepest", "variant": "minimal" }
        });
        for _ in 0..15 {
            nested = json!({
                "type": "Columns",
                "props": { "layout": "1/2+1/2" },
                "zones": { "zone-0": [nested] }
            });
        }
        let page: PuckPage = serde_json::from_value(json!({
            "content": [nested]
        }))
        .unwrap();

        let result = render_puck_page(&page, &tera);
        assert!(result.is_err());
    }

    #[test]
    fn accessibility_blocks_missing_alt() {
        let tera = test_tera();
        let page: PuckPage = serde_json::from_value(json!({
            "content": [{
                "type": "ContentFeature",
                "props": {
                    "title": "Feature",
                    "imageUrl": "https://example.com/photo.jpg"
                }
            }]
        }))
        .unwrap();

        let html = render_puck_page(&page, &tera).unwrap();
        assert!(html.contains("a11y error"), "should block: {html}");
        assert!(!html.contains("pb-feature"));
    }

    #[test]
    fn accessibility_allows_decorative_images() {
        let result = validate_accessibility(
            "ContentFeature",
            &json!({
                "imageUrl": "https://example.com/bg.jpg",
                "isDecorative": true
            }),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn accessibility_blocks_h1_heading() {
        let result = validate_accessibility("Hero", &json!({ "headingLevel": 1 }));
        assert!(result.is_err());
    }

    #[test]
    fn accessibility_allows_h2_heading() {
        let result = validate_accessibility("Hero", &json!({ "headingLevel": 2 }));
        assert!(result.is_ok());
    }

    // --- Heading hierarchy tests ---

    #[test]
    fn heading_hierarchy_skip_detected() {
        let html = r#"<h2>Title</h2><p>text</p><h5>Subtitle</h5>"#;
        let warnings = validate_heading_hierarchy(html);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("H2 → H5"), "got: {}", warnings[0]);
    }

    #[test]
    fn heading_hierarchy_valid() {
        let html = r#"<h2>Title</h2><h3>Sub</h3><h3>Sub2</h3><h2>Another</h2>"#;
        let warnings = validate_heading_hierarchy(html);
        assert!(warnings.is_empty());
    }

    #[test]
    fn heading_hierarchy_empty_page() {
        let warnings = validate_heading_hierarchy("<p>No headings here</p>");
        assert!(warnings.is_empty());
    }

    #[test]
    fn heading_hierarchy_going_up_is_fine() {
        // H4 → H2 (going up) is valid — only going deeper can skip
        let html = r#"<h2>A</h2><h3>B</h3><h4>C</h4><h2>D</h2>"#;
        let warnings = validate_heading_hierarchy(html);
        assert!(warnings.is_empty());
    }

    // --- Markdown heading rewrite tests ---

    #[test]
    fn markdown_heading_rewrite_promotes_h1_to_h2() {
        let result = rewrite_markdown_headings("# Title\n## Sub\n### Deep", 2);
        assert!(result.starts_with("## Title"), "got: {result}");
        assert!(result.contains("## Sub"));
        assert!(result.contains("### Deep"));
    }

    #[test]
    fn markdown_heading_rewrite_no_change_above_min() {
        let result = rewrite_markdown_headings("## Title\n### Sub", 2);
        assert_eq!(result, "## Title\n### Sub");
    }

    #[test]
    fn markdown_heading_rewrite_no_change_when_min_is_1() {
        let result = rewrite_markdown_headings("# Title", 1);
        assert_eq!(result, "# Title");
    }

    #[test]
    fn markdown_heading_rewrite_in_text_block() {
        let tera = test_tera();
        let page: PuckPage = serde_json::from_value(json!({
            "content": [{
                "type": "TextBlock",
                "props": { "content": "# This Should Be H2\n\nParagraph text." }
            }]
        }))
        .unwrap();

        let html = render_puck_page(&page, &tera).unwrap();
        // The template uses markdown(min_heading=2), so # becomes ##
        assert!(
            html.contains("<h2>") && !html.contains("<h1>"),
            "H1 should be rewritten to H2: {html}"
        );
    }
}
