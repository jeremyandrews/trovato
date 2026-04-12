//! Puck JSON → Tera renderer for the page builder POC.
//!
//! Takes a Puck-format JSON file and renders it to HTML using Tera templates,
//! pulldown-cmark for Markdown, and Ammonia for sanitization.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Top-level Puck page structure.
#[derive(Deserialize, Debug)]
struct PuckPage {
    root: Option<PuckRoot>,
    content: Vec<PuckComponent>,
}

/// Root-level page metadata.
#[derive(Deserialize, Debug)]
struct PuckRoot {
    props: Option<serde_json::Value>,
}

/// A single Puck component with optional child zones.
#[derive(Deserialize, Debug)]
struct PuckComponent {
    #[serde(rename = "type")]
    component_type: String,
    props: serde_json::Value,
    #[serde(default)]
    zones: HashMap<String, Vec<PuckComponent>>,
}

/// Convert a Puck component type name to a Tera template file name.
///
/// "Hero" → "components/hero.tera"
/// "TextBlock" → "components/text-block.tera"
fn template_name(component_type: &str) -> String {
    // Convert PascalCase to kebab-case
    let mut kebab = String::new();
    for (i, c) in component_type.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            kebab.push('-');
        }
        kebab.push(c.to_ascii_lowercase());
    }
    format!("components/{kebab}.tera")
}

/// Render Markdown to sanitized HTML.
fn render_markdown(source: &str) -> String {
    use pulldown_cmark::{Options, Parser, html};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(source, options);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);
    html_output
}

/// Configure Ammonia for page builder output.
///
/// Allows standard HTML tags plus the `style` attribute on specific elements
/// (needed for `background-image` on Hero and `gap` on Columns).
fn sanitize(html: &str) -> String {
    let mut builder = ammonia::Builder::default();
    // Allow class and style attributes (needed for component styling)
    builder.add_generic_attributes(["class", "style"]);
    // Allow section element (used by Hero)
    builder.add_tags(["section"]);
    builder.clean(html).to_string()
}

/// Render a single Puck component to HTML.
fn render_component(component: &PuckComponent, tera: &tera::Tera) -> Result<String> {
    let tmpl = template_name(&component.component_type);
    let mut context = tera::Context::new();

    // Flatten props into the Tera context
    if let Some(obj) = component.props.as_object() {
        for (key, value) in obj {
            // Special handling for TextBlock: render Markdown
            if component.component_type == "TextBlock" && key == "content" {
                if let Some(md) = value.as_str() {
                    context.insert("rendered_content", &render_markdown(md));
                }
            }
            context.insert(key, value);
        }
    }

    // Special handling for Columns: convert layout prop to CSS class
    if component.component_type == "Columns" {
        if let Some(layout) = component.props.get("layout").and_then(|v| v.as_str()) {
            // "2/3+1/3" → "2-3-1-3"
            let layout_class = layout.replace('/', "-").replace('+', "-");
            context.insert("layout_class", &layout_class);
        }

        // Render zones in order: zone-0, zone-1, zone-2, ...
        let max_zones = component.zones.len();
        let mut rendered_zones: Vec<String> = Vec::with_capacity(max_zones);

        for i in 0..max_zones {
            let zone_key = format!("zone-{i}");
            if let Some(children) = component.zones.get(&zone_key) {
                let rendered: Vec<String> = children
                    .iter()
                    .map(|child| render_component(child, tera))
                    .collect::<Result<Vec<_>>>()?;
                rendered_zones.push(rendered.join("\n"));
            } else {
                rendered_zones.push(String::new());
            }
        }

        context.insert("zones", &rendered_zones);
    }

    tera.render(&tmpl, &context)
        .with_context(|| format!("failed to render component '{}'", component.component_type))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        anyhow::bail!("Usage: pb-renderer <fixture.json>");
    }

    let json_path = &args[1];
    let start = Instant::now();

    // Load and parse the Puck JSON
    let json_str = std::fs::read_to_string(json_path)
        .with_context(|| format!("failed to read {json_path}"))?;
    let page: PuckPage =
        serde_json::from_str(&json_str).context("failed to parse Puck JSON")?;

    // Initialize Tera from templates directory
    let templates_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/**/*");
    let templates_glob = templates_dir
        .to_str()
        .context("invalid templates path")?;
    let tera = tera::Tera::new(templates_glob).context("failed to load Tera templates")?;

    // Render and sanitize each component individually.
    // In production, the page wrapper is kernel-controlled (trusted); only
    // component bodies need sanitization. Ammonia strips <html>, <head>,
    // <style> etc. so sanitizing the full page would destroy the wrapper.
    let mut body_parts: Vec<String> = Vec::with_capacity(page.content.len());
    for component in &page.content {
        let html = render_component(component, &tera)?;
        body_parts.push(sanitize(&html));
    }
    let body_html = body_parts.join("\n");

    // Extract page title from root props
    let title = page
        .root
        .as_ref()
        .and_then(|r| r.props.as_ref())
        .and_then(|p| p.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("Page");

    // Render the page wrapper (trusted kernel template — not sanitized)
    let mut page_context = tera::Context::new();
    page_context.insert("title", title);
    page_context.insert("content", &body_html);
    let full_html = tera
        .render("page.tera", &page_context)
        .context("failed to render page template")?;

    let elapsed = start.elapsed();
    eprintln!(
        "Rendered {} components in {:.2}ms",
        page.content.len(),
        elapsed.as_secs_f64() * 1000.0
    );

    print!("{full_html}");
    Ok(())
}
