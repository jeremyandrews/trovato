//! Puck-based page builder renderer.
//!
//! Parses Puck JSON component trees and renders them to HTML using Tera
//! templates. Each component's definition in the [`ComponentRegistry`] names
//! its template, resolved on the kernel's template search path, so a plugin's
//! `templates/` directory or a theme on `TEMPLATES_DIR` supplies it.
//! Zone-based nesting allows arbitrary component composition (e.g., Columns
//! containing CTA + TextBlock children).
//!
//! A template receives the component's declared fields only, each normalized
//! to its type: a select value outside its options, an unsafe URL or a value
//! of the wrong type is left out, so the template's own default applies. It
//! also receives `variant_classes`, the classes the chosen select options add,
//! every one from the component's declared `classes`.
//!
//! The page's root values (the built in `title` and every page root field
//! the enabled plugins declare) are normalized the same way, declared
//! defaults filled in, into [`RenderedPage::root`]: what page, region and
//! tile templates receive as `page_root` ([`page_root_values`]).
//!
//! Security: each component is sanitized individually via Ammonia before
//! assembly, and the allowlist carries no `style` attribute: a component
//! expresses a variant as a declared class, never as inline CSS. The page
//! wrapper template is kernel-controlled and trusted.

use std::collections::HashMap;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::page_builder_components::{
    ComponentDefinition, ComponentRegistry, FieldDefinition, FieldKind, MAX_ZONE_DEPTH,
    ReferenceTarget, is_safe_url, root_values,
};

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

/// The sanitizer every component's HTML passes through. Built once: it is the
/// same for every component and every page.
static SANITIZER: LazyLock<ammonia::Builder<'static>> = LazyLock::new(build_sanitizer);

/// The standard components' stylesheet, linked whenever a page builder
/// component renders.
pub const STANDARD_STYLESHEET: &str = "/static/css/page-builder.css";

/// Maximum zone depth to prevent infinite recursion from malformed JSON.
const MAX_RECURSION_DEPTH: usize = MAX_ZONE_DEPTH;

/// A full Puck page as stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PuckPage {
    /// Page-level metadata.
    pub root: Option<PuckRoot>,
    /// Top-level component list.
    pub content: Vec<PuckComponent>,
}

/// Page-level values from Puck's root config.
///
/// `title` is built in. Every other key is kept in `fields` as stored, in
/// either shape the editor's Puck may write (directly under `root`, or under
/// `root.props`); [`PuckRoot::values`] reads both. Nothing here is validated:
/// a stored page always renders, and only the values a page root field
/// declares reach a template ([`page_root_values`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PuckRoot {
    /// Page title (may be used by the page template).
    #[serde(default)]
    pub title: Option<String>,
    /// Every other root key, as stored.
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

impl PuckRoot {
    /// The root's values, `title` included, from either stored shape: a key
    /// under `props` takes precedence over the same key directly under root.
    pub fn values(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut flat = self.fields.clone();
        if let Some(title) = &self.title {
            flat.insert("title".into(), serde_json::Value::String(title.clone()));
        }
        root_values(&serde_json::Value::Object(flat))
    }
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

/// What a page renders to: the HTML, the stylesheets the components that
/// rendered declared, each once, in first-use order, and the page's root
/// values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderedPage {
    /// The concatenated, sanitized component HTML.
    pub html: String,
    /// Stylesheet URLs the page must link for this HTML.
    pub stylesheets: Vec<String>,
    /// The page's root values as templates receive them, `page_root`: see
    /// [`page_root_values`].
    pub root: serde_json::Map<String, serde_json::Value>,
}

/// A reference a page's components hold, to be resolved before rendering.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReferenceKey {
    /// What it refers to.
    pub target: ReferenceTarget,
    /// The item UUID or gather query id, as stored.
    pub id: String,
    /// For a gather, how many rows the template receives.
    pub limit: u32,
}

impl ReferenceKey {
    fn lookup_key(target: ReferenceTarget, id: &str) -> String {
        match target {
            ReferenceTarget::Item => format!("item:{id}"),
            ReferenceTarget::Gather => format!("gather:{id}"),
        }
    }
}

/// Resolved references, by [`ReferenceKey`]: what a template receives for
/// each. A reference the viewer may not see, or that no longer exists, is
/// simply absent, and the template renders without it.
#[derive(Debug, Clone, Default)]
pub struct ResolvedReferences(HashMap<String, serde_json::Value>);

impl ResolvedReferences {
    /// Record what `key` resolved to.
    pub fn insert(&mut self, key: &ReferenceKey, value: serde_json::Value) {
        self.0
            .insert(ReferenceKey::lookup_key(key.target, &key.id), value);
    }

    fn get(&self, target: ReferenceTarget, id: &str) -> Option<&serde_json::Value> {
        self.0.get(&ReferenceKey::lookup_key(target, id))
    }
}

/// Every reference the page's components hold, each once, so a caller with
/// the request's services can resolve them (see
/// `routes::helpers::render_page_builder_field`).
pub fn collect_references(page: &PuckPage, registry: &ComponentRegistry) -> Vec<ReferenceKey> {
    fn walk(
        component: &PuckComponent,
        registry: &ComponentRegistry,
        depth: usize,
        out: &mut Vec<ReferenceKey>,
    ) {
        if depth > MAX_RECURSION_DEPTH {
            return;
        }
        if let Some(def) = registry.get(&component.component_type) {
            collect_in(&def.fields, &component.props, out);
        }
        for children in component.zones.values() {
            for child in children {
                walk(child, registry, depth + 1, out);
            }
        }
    }
    fn collect_in(
        fields: &[FieldDefinition],
        props: &serde_json::Value,
        out: &mut Vec<ReferenceKey>,
    ) {
        for field in fields {
            let Some(value) = props.get(&field.name) else {
                continue;
            };
            match &field.kind {
                FieldKind::Reference { target, limit, .. } => {
                    if let Some(id) = value.as_str().filter(|s| !s.is_empty()) {
                        let key = ReferenceKey {
                            target: *target,
                            id: id.to_string(),
                            limit: *limit,
                        };
                        if !out.contains(&key) {
                            out.push(key);
                        }
                    }
                }
                FieldKind::List { fields, .. } => {
                    for item in value.as_array().into_iter().flatten() {
                        collect_in(fields, item, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    if let Some(root) = &page.root {
        collect_in(
            registry.root_field_definitions(),
            &serde_json::Value::Object(root.values()),
            &mut out,
        );
    }
    for component in &page.content {
        walk(component, registry, 0, &mut out);
    }
    out
}

/// Build the Ammonia sanitizer for page builder components.
///
/// Allows `class` (components style themselves with their own and their
/// declared variant classes), the semantic tags components use, and standard
/// formatting. No `style`: inline CSS from a prop is how a page builder
/// becomes a CSS injection surface (BL-67), so it is not allowed at all.
fn build_sanitizer() -> ammonia::Builder<'static> {
    let mut builder = ammonia::Builder::default();
    builder.add_generic_attributes(["class"]);
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
    // A link field may open in a new tab; `_blank` is the one value allowed,
    // and ammonia's default rel="noopener noreferrer" goes with it.
    builder.add_tag_attribute_values("a", "target", ["_blank"]);
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
    def: Option<&ComponentDefinition>,
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

    // Rule 1b: the same for a declared image field, at the top level or in a
    // list entry.
    if let Some(def) = def
        && let Some(path) = image_without_alt(&def.fields, props, "")
    {
        return Err(format!(
            "component '{component_type}' field '{path}' has an image but no alt text \
             (and is not marked decorative) — WCAG 1.1.1"
        ));
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

/// The path of the first declared image field holding an image with neither
/// alt text nor `decorative`.
fn image_without_alt(
    fields: &[FieldDefinition],
    props: &serde_json::Value,
    prefix: &str,
) -> Option<String> {
    for field in fields {
        let Some(value) = props.get(&field.name) else {
            continue;
        };
        let path = if prefix.is_empty() {
            field.name.clone()
        } else {
            format!("{prefix}.{}", field.name)
        };
        match &field.kind {
            FieldKind::Image { .. } => {
                let src = value.get("src").and_then(|v| v.as_str()).unwrap_or("");
                let alt = value.get("alt").and_then(|v| v.as_str()).unwrap_or("");
                let decorative = value
                    .get("decorative")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !src.is_empty() && alt.trim().is_empty() && !decorative {
                    return Some(path);
                }
            }
            FieldKind::List { fields, .. } => {
                for (i, item) in value.as_array().into_iter().flatten().enumerate() {
                    if let Some(found) = image_without_alt(fields, item, &format!("{path}[{i}]")) {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Render a full Puck page to HTML.
///
/// Returns the rendered body HTML (component outputs concatenated) and the
/// stylesheets the rendered components declared. The caller wraps the HTML in
/// the page-level Tera template and links the stylesheets in its head.
///
/// After rendering, validates heading hierarchy across the full page
/// and logs warnings for skipped heading levels.
pub fn render_puck_page(
    page: &PuckPage,
    tera: &tera::Tera,
    registry: &ComponentRegistry,
    references: &ResolvedReferences,
) -> Result<RenderedPage> {
    let renderer = Renderer {
        tera,
        registry,
        references,
    };
    let root = page_root_values(page.root.as_ref(), registry, references);
    let mut stylesheets = vec![STANDARD_STYLESHEET.to_string()];
    let mut parts = Vec::with_capacity(page.content.len());
    for component in &page.content {
        let html = renderer.render_component(component, 0, &mut stylesheets)?;
        parts.push(html);
    }
    let rendered = parts.join("\n");

    // Post-render heading hierarchy validation (warnings, not errors)
    let hierarchy_warnings = validate_heading_hierarchy(&rendered);
    for w in &hierarchy_warnings {
        tracing::warn!(warning = %w, "page builder heading hierarchy issue");
    }

    if rendered.trim().is_empty() {
        stylesheets.clear();
    }

    Ok(RenderedPage {
        html: rendered,
        stylesheets,
        root,
    })
}

/// A page's root values as page, region and tile templates receive them, in
/// the context variable `page_root`.
///
/// - `title`: the root's title, when it is text;
/// - one key per page root field the enabled plugins declare: the page's
///   value, or the field's declared default when the page has none (absent,
///   `null` or empty text), normalized to the field's type exactly as a
///   component prop is (a select value outside its options, an unsafe URL or
///   a value of the wrong type is left out; a `rich_text` field adds
///   `<name>_html`, the sanitized rendering);
/// - `variant_classes`: the classes the chosen select options add, space
///   separated, each one a class a root file declared.
///
/// Root keys no enabled plugin declares are not passed on. `None` (a page
/// with no root, or no page builder field at all) gives the defaults.
pub fn page_root_values(
    root: Option<&PuckRoot>,
    registry: &ComponentRegistry,
    references: &ResolvedReferences,
) -> serde_json::Map<String, serde_json::Value> {
    let values = root.map(PuckRoot::values).unwrap_or_default();
    let fields = registry.root_field_definitions();
    let mut effective = serde_json::Map::new();
    for field in fields {
        let value = values
            .get(&field.name)
            .filter(|v| !v.is_null() && v.as_str() != Some(""))
            .or(field.default.as_ref());
        if let Some(value) = value {
            effective.insert(field.name.clone(), value.clone());
        }
    }
    let effective = serde_json::Value::Object(effective);
    let mut out = match normalize_props(fields, &effective, references) {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    if let Some(title) = values.get("title").filter(|t| t.is_string()) {
        out.insert("title".into(), title.clone());
    }
    out.insert(
        "variant_classes".into(),
        serde_json::Value::String(chosen_classes(fields, registry.root_classes(), &effective)),
    );
    out
}

/// [`page_root_values`] for a page without page builder content: every
/// declared default, and no title.
pub fn page_root_defaults(registry: &ComponentRegistry) -> serde_json::Value {
    serde_json::Value::Object(page_root_values(
        None,
        registry,
        &ResolvedReferences::default(),
    ))
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

/// One page's rendering inputs.
struct Renderer<'a> {
    tera: &'a tera::Tera,
    registry: &'a ComponentRegistry,
    references: &'a ResolvedReferences,
}

impl Renderer<'_> {
    /// Render a single Puck component to HTML, recursively rendering zone children.
    fn render_component(
        &self,
        component: &PuckComponent,
        depth: usize,
        stylesheets: &mut Vec<String>,
    ) -> Result<String> {
        if depth > MAX_RECURSION_DEPTH {
            anyhow::bail!(
                "page builder recursion depth exceeded ({MAX_RECURSION_DEPTH}) \
                 for component type '{}'",
                component.component_type
            );
        }

        // Skip unknown component types gracefully: a page saved while a
        // plugin was enabled keeps rendering, without that component, after it
        // is disabled.
        let Some(def) = self.registry.get(&component.component_type) else {
            tracing::warn!(
                component_type = %component.component_type,
                "unknown page builder component type, skipping"
            );
            return Ok(String::new());
        };
        if self.tera.get_template(&def.template).is_err() {
            tracing::warn!(
                component_type = %component.component_type,
                template = %def.template,
                "page builder component template not found, skipping"
            );
            return Ok(String::new());
        }

        // Accessibility validation
        match validate_accessibility(&component.component_type, &component.props, Some(def)) {
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

        // Build the Tera context from the declared fields
        let mut context = tera::Context::new();
        if let Some(obj) =
            normalize_props(&def.fields, &component.props, self.references).as_object()
        {
            for (key, value) in obj {
                context.insert(key, value);
            }
        }
        context.insert("variant_classes", &variant_classes(def, &component.props));

        // Recursively render zone children BEFORE rendering parent
        if !component.zones.is_empty() {
            let mut rendered_zones: HashMap<String, Vec<String>> = HashMap::new();

            // Collect all zones by iterating the actual keys
            for (zone_key, children) in &component.zones {
                let zone_html: Vec<String> = children
                    .iter()
                    .map(|child| self.render_component(child, depth + 1, stylesheets))
                    .collect::<Result<Vec<_>>>()?;
                rendered_zones.insert(zone_key.clone(), zone_html);
            }

            context.insert("zones", &rendered_zones);
        }

        // Render template
        let html = self.tera.render(&def.template, &context).with_context(|| {
            format!("failed to render component '{}'", component.component_type)
        })?;

        for sheet in &def.stylesheets {
            if !stylesheets.contains(sheet) {
                stylesheets.push(sheet.clone());
            }
        }

        // Sanitize per-component
        Ok(SANITIZER.clean(&html).to_string())
    }
}

/// The template's view of `props`: each declared field, normalized to its
/// type, and nothing else. A rich text field adds `<name>_html`.
fn normalize_props(
    fields: &[FieldDefinition],
    props: &serde_json::Value,
    references: &ResolvedReferences,
) -> serde_json::Value {
    use serde_json::Value;
    let mut out = serde_json::Map::new();
    for field in fields {
        let Some(value) = props.get(&field.name).filter(|v| !v.is_null()) else {
            continue;
        };
        let normalized = match &field.kind {
            FieldKind::Text { .. } => value.is_string().then(|| value.clone()),
            FieldKind::RichText { min_heading, .. } => {
                if let Some(text) = value.as_str() {
                    out.insert(
                        format!("{}_html", field.name),
                        Value::String(crate::theme::render_markdown(
                            text,
                            usize::from(*min_heading),
                        )),
                    );
                    Some(value.clone())
                } else {
                    None
                }
            }
            FieldKind::Number { .. } => value.is_number().then(|| value.clone()),
            FieldKind::HeadingLevel => value
                .as_u64()
                .filter(|l| (2..=6).contains(l))
                .map(Value::from),
            FieldKind::Boolean => value.is_boolean().then(|| value.clone()),
            FieldKind::Url => value
                .as_str()
                .filter(|u| is_safe_url(u))
                .map(|u| Value::String(u.to_string())),
            FieldKind::Select { options } => value
                .as_str()
                .filter(|v| options.iter().any(|o| o.value == *v))
                .map(|v| Value::String(v.to_string())),
            FieldKind::Image { .. } => {
                let src = value.get("src").and_then(Value::as_str).unwrap_or("");
                if src.is_empty() || !is_safe_url(src) {
                    None
                } else {
                    let decorative = value
                        .get("decorative")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let alt = if decorative {
                        ""
                    } else {
                        value.get("alt").and_then(Value::as_str).unwrap_or("")
                    };
                    Some(serde_json::json!({
                        "src": src,
                        "alt": alt,
                        "decorative": decorative,
                    }))
                }
            }
            FieldKind::Link { .. } => {
                let url = value.get("url").and_then(Value::as_str).unwrap_or("");
                if url.is_empty() || !is_safe_url(url) {
                    None
                } else {
                    Some(serde_json::json!({
                        "url": url,
                        "label": value.get("label").and_then(Value::as_str).unwrap_or(""),
                        "new_tab": value.get("new_tab").and_then(Value::as_bool).unwrap_or(false),
                    }))
                }
            }
            FieldKind::List { fields, .. } => value.as_array().map(|items| {
                Value::Array(
                    items
                        .iter()
                        .filter(|i| i.is_object())
                        .map(|i| normalize_props(fields, i, references))
                        .collect(),
                )
            }),
            // An item of another type than the field names is left out,
            // as one the viewer may not see is.
            FieldKind::Reference {
                target, item_type, ..
            } => value
                .as_str()
                .and_then(|id| references.get(*target, id))
                .filter(|resolved| match item_type {
                    Some(wanted) => {
                        resolved.get("type").and_then(Value::as_str) == Some(wanted.as_str())
                    }
                    None => true,
                })
                .cloned(),
        };
        if let Some(v) = normalized {
            out.insert(field.name.clone(), v);
        }
    }
    Value::Object(out)
}

/// The classes the component's chosen select options add, space separated,
/// each checked against the component's declared `classes`.
fn variant_classes(def: &ComponentDefinition, props: &serde_json::Value) -> String {
    chosen_classes(&def.fields, &def.classes, props)
}

/// The classes the chosen options of `fields`' select fields add, each one
/// of `declared`, once, in field order.
fn chosen_classes(
    fields: &[FieldDefinition],
    declared: &[String],
    props: &serde_json::Value,
) -> String {
    let mut classes: Vec<&str> = Vec::new();
    for field in fields {
        let FieldKind::Select { options } = &field.kind else {
            continue;
        };
        let Some(choice) = props.get(&field.name).and_then(|v| v.as_str()) else {
            continue;
        };
        if let Some(class) = options
            .iter()
            .find(|o| o.value == choice)
            .and_then(|o| o.class.as_deref())
            && declared.iter().any(|c| c == class)
            && !classes.contains(&class)
        {
            classes.push(class);
        }
    }
    classes.join(" ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Render with the standard registry and no references, returning the HTML.
    fn render(page: &PuckPage, tera: &tera::Tera) -> String {
        render_puck_page(
            page,
            tera,
            &ComponentRegistry::new(),
            &ResolvedReferences::default(),
        )
        .unwrap()
        .html
    }

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

        let html = render(&page, &tera);
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

        let html = render(&page, &tera);
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

        let html = render(&page, &tera);
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

        let html = render(&page, &tera);
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

        let html = render(&page, &tera);
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

        let result = render_puck_page(
            &page,
            &tera,
            &ComponentRegistry::new(),
            &ResolvedReferences::default(),
        );
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

        let html = render(&page, &tera);
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
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn accessibility_blocks_h1_heading() {
        let result = validate_accessibility("Hero", &json!({ "headingLevel": 1 }), None);
        assert!(result.is_err());
    }

    #[test]
    fn accessibility_allows_h2_heading() {
        let result = validate_accessibility("Hero", &json!({ "headingLevel": 2 }), None);
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

        let html = render(&page, &tera);
        // The template uses markdown(min_heading=2), so # becomes ##
        assert!(
            html.contains("<h2>") && !html.contains("<h1>"),
            "H1 should be rewritten to H2: {html}"
        );
    }

    // --- The registry-driven renderer ---

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/page_builder")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// The kernel's real templates and filters, as a running server has them.
    fn engine() -> crate::theme::ThemeEngine {
        let templates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates");
        crate::theme::ThemeEngine::new(&[templates], None, Default::default()).unwrap()
    }

    /// A page using every standard component, saved before components were
    /// declared in files, still validates and renders byte for byte the HTML
    /// it rendered then (`standard-page-v0.106.html` was written by the
    /// renderer as it was before this change).
    #[test]
    fn a_page_saved_by_0_106_validates_and_renders_the_same_html() {
        let json: serde_json::Value =
            serde_json::from_str(&fixture("standard-page-v0.106.json")).unwrap();
        let registry = ComponentRegistry::new();
        let errors = registry.validate_page(&json);
        assert!(errors.is_empty(), "{errors:?}");

        let page: PuckPage = serde_json::from_value(json).unwrap();
        let rendered = render_puck_page(
            &page,
            engine().tera(),
            &registry,
            &ResolvedReferences::default(),
        )
        .unwrap();
        assert_eq!(rendered.html, fixture("standard-page-v0.106.html"));
        assert_eq!(rendered.stylesheets, vec![STANDARD_STYLESHEET]);
        // Its root is the title alone, and templates receive just that.
        assert_eq!(
            serde_json::Value::Object(rendered.root),
            json!({ "title": "A page saved by v0.106.0", "variant_classes": "" })
        );

        // With a plugin declaring page root fields enabled, the same page
        // still validates, renders the same HTML, and gets their defaults.
        let registry = root_registry();
        assert!(registry.validate_page(&json_fixture()).is_empty());
        let page: PuckPage = serde_json::from_value(json_fixture()).unwrap();
        let rendered = render_puck_page(
            &page,
            engine().tera(),
            &registry,
            &ResolvedReferences::default(),
        )
        .unwrap();
        assert_eq!(rendered.html, fixture("standard-page-v0.106.html"));
        assert_eq!(rendered.root["footer_mode"], "default");
    }

    fn json_fixture() -> serde_json::Value {
        serde_json::from_str(&fixture("standard-page-v0.106.json")).unwrap()
    }

    const ROOT: &str = r#"
classes = ["page--no-footer-cta"]
[[fields]]
name = "footer_mode"
type = "select"
default = "default"
options = [
  { value = "default" },
  { value = "hide", class = "page--no-footer-cta" },
  { value = "custom" },
]
[[fields]]
name = "footer_text"
type = "rich_text"
[[fields]]
name = "footer_link"
type = "url"
"#;

    fn root_registry() -> ComponentRegistry {
        let dir = std::env::temp_dir().join(format!("trovato-pb-root-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("components")).unwrap();
        std::fs::write(dir.join("components/page.root.toml"), ROOT).unwrap();
        let registry = ComponentRegistry::load(&[
            crate::content::page_builder_components::PluginComponentSource {
                name: "site".into(),
                dir: dir.clone(),
            },
        ])
        .unwrap();
        std::fs::remove_dir_all(dir).ok();
        registry
    }

    fn root_of(page: serde_json::Value, registry: &ComponentRegistry) -> serde_json::Value {
        let page: PuckPage = serde_json::from_value(page).unwrap();
        serde_json::Value::Object(page_root_values(
            page.root.as_ref(),
            registry,
            &ResolvedReferences::default(),
        ))
    }

    #[test]
    fn page_root_values_are_normalized_with_defaults() {
        let registry = root_registry();
        // No value: the declared default.
        assert_eq!(
            root_of(
                json!({ "root": { "title": "T" }, "content": [] }),
                &registry
            ),
            json!({ "title": "T", "footer_mode": "default", "variant_classes": "" })
        );
        // A chosen option adds its declared class.
        assert_eq!(
            root_of(
                json!({ "root": { "footer_mode": "hide" }, "content": [] }),
                &registry
            ),
            json!({ "footer_mode": "hide", "variant_classes": "page--no-footer-cta" })
        );
        // Nested under props, as current Puck keeps it; rich text is rendered
        // and sanitized; an unsafe URL, an unknown option and an undeclared
        // key never reach a template.
        let root = root_of(
            json!({ "root": { "props": {
                "footer_mode": "custom",
                "footer_text": "# Call us\n\n<script>x</script>\n\nCall **now**",
                "footer_link": "javascript:alert(1)",
                "colour": "red"
            } }, "content": [] }),
            &registry,
        );
        assert_eq!(root["footer_mode"], "custom");
        let html = root["footer_text_html"].as_str().unwrap();
        assert!(html.contains("<h2>Call us</h2>"), "{html}");
        assert!(
            html.contains("<strong>now</strong>") && !html.contains("<script"),
            "{html}"
        );
        assert!(root.get("footer_link").is_none(), "{root}");
        assert!(root.get("colour").is_none(), "{root}");
        let root = root_of(
            json!({ "root": { "footer_mode": "blink" }, "content": [] }),
            &registry,
        );
        assert!(root.get("footer_mode").is_none(), "{root}");
        // No page builder content at all: the defaults.
        assert_eq!(
            page_root_defaults(&registry),
            json!({ "footer_mode": "default", "variant_classes": "" })
        );
        assert_eq!(
            page_root_defaults(&ComponentRegistry::new()),
            json!({ "variant_classes": "" })
        );
    }

    #[test]
    fn the_old_style_props_no_longer_reach_the_page() {
        let page: PuckPage = serde_json::from_value(json!({ "content": [
            { "type": "Hero", "props": {
                "title": "T", "variant": "standard", "backgroundImage": "/bg.jpg", "imageAlt": "x",
                "backgroundColor": "red;background:url(x)"
            } },
            { "type": "Columns", "props": { "layout": "1/2+1/2", "gap": "9rem" } },
            { "type": "SectionWrapper", "props": { "backgroundColor": "#000" } },
            { "type": "Cta", "props": { "heading": "x", "backgroundColor": "blue" } }
        ] }))
        .unwrap();
        let html = render(&page, engine().tera());
        assert!(!html.contains("style"), "{html}");
        assert!(!html.contains("9rem") && !html.contains("red"), "{html}");
        // The standard hero's background image is an <img> behind the text.
        assert!(
            html.contains(r#"<img class="pb-hero__background" src="/bg.jpg" alt="""#),
            "{html}"
        );
    }

    #[test]
    fn a_background_variant_adds_its_declared_class() {
        let page: PuckPage = serde_json::from_value(json!({ "content": [
            { "type": "Cta", "props": { "heading": "x", "background": "dark" } },
            { "type": "Columns", "props": { "layout": "1/2+1/2", "gapSize": "large" } },
            { "type": "SectionWrapper", "props": { "background": "brand" } },
            { "type": "Hero", "props": { "title": "T", "background": "light" } },
            { "type": "Cta", "props": { "heading": "y", "background": "neon" } }
        ] }))
        .unwrap();
        let html = render(&page, engine().tera());
        assert!(
            html.contains(r#"class="pb-cta pb-cta--inline pb-bg--dark""#),
            "{html}"
        );
        assert!(html.contains("pb-columns--gap-large"), "{html}");
        assert!(
            html.contains("pb-section--width-default pb-bg--brand"),
            "{html}"
        );
        assert!(html.contains("pb-hero--standard pb-bg--light"), "{html}");
        assert!(!html.contains("neon"), "{html}");
    }

    const TILES: &str = r#"
type = "Tiles"
label = "Tiles"
category = "marketing"
classes = ["tiles--dark"]
stylesheets = ["tiles.css"]
[[fields]]
name = "tone"
type = "select"
options = [{ value = "light" }, { value = "dark", class = "tiles--dark" }]
[[fields]]
name = "intro"
type = "rich_text"
[[fields]]
name = "hero"
type = "image"
[[fields]]
name = "tiles"
type = "list"
  [[fields.fields]]
  name = "link"
  type = "link"
"#;

    fn tiles_registry() -> ComponentRegistry {
        let dir = std::env::temp_dir().join(format!("trovato-pb-render-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("components")).unwrap();
        std::fs::create_dir_all(dir.join("static")).unwrap();
        std::fs::write(dir.join("static/tiles.css"), "").unwrap();
        std::fs::write(dir.join("components/tiles.component.toml"), TILES).unwrap();
        let registry = ComponentRegistry::load(&[
            crate::content::page_builder_components::PluginComponentSource {
                name: "site".into(),
                dir: dir.clone(),
            },
        ])
        .unwrap();
        std::fs::remove_dir_all(dir).ok();
        registry
    }

    fn tiles_tera() -> tera::Tera {
        let mut tera = engine().tera().clone();
        tera.add_raw_template(
            "pb/tiles.html",
            r#"<section class="tiles {{ variant_classes }}" style="color:red" onclick="x()">{{ intro_html | safe }}{% if hero %}<img src="{{ hero.src }}" alt="{{ hero.alt }}">{% endif %}{% for t in tiles %}{% if t.link %}<a href="{{ t.link.url }}"{% if t.link.new_tab %} target="_blank"{% endif %}>{{ t.link.label }}</a>{% endif %}{% endfor %}</section>"#,
        )
        .unwrap();
        tera
    }

    #[test]
    fn a_plugin_component_renders_its_declared_fields() {
        let registry = tiles_registry();
        let page: PuckPage = serde_json::from_value(json!({ "content": [
            { "type": "Tiles", "props": {
                "tone": "dark",
                "intro": "# Big\n\n<script>x()</script>\n\n*hi*",
                "hero": { "src": "/files/h.png", "alt": "A view", "decorative": false },
                "tiles": [
                    { "link": { "url": "/a", "label": "A", "new_tab": true } },
                    { "link": { "url": "javascript:alert(1)", "label": "B" } }
                ],
                "undeclared": "<b>never</b>"
            } }
        ] }))
        .unwrap();
        let rendered = render_puck_page(
            &page,
            &tiles_tera(),
            &registry,
            &ResolvedReferences::default(),
        )
        .unwrap();
        let html = rendered.html;
        assert!(html.contains(r#"class="tiles tiles--dark""#), "{html}");
        assert!(!html.contains("style="), "{html}");
        assert!(!html.contains("onclick"), "{html}");
        assert!(html.contains("<h2>Big</h2>"), "{html}");
        assert!(!html.contains("<script"), "{html}");
        assert!(html.contains("<em>hi</em>"), "{html}");
        assert!(
            html.contains(r#"<img src="/files/h.png" alt="A view">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<a href="/a" target="_blank" rel="noopener noreferrer">A</a>"#),
            "{html}"
        );
        assert!(!html.contains("javascript"), "{html}");
        assert!(!html.contains("never"), "{html}");
        assert_eq!(
            rendered.stylesheets,
            vec![STANDARD_STYLESHEET, "/static/plugins/site/tiles.css"]
        );
    }

    #[test]
    fn a_declared_image_without_alt_is_not_rendered() {
        let registry = tiles_registry();
        let page: PuckPage = serde_json::from_value(json!({ "content": [
            { "type": "Tiles", "props": { "hero": { "src": "/files/h.png", "alt": "" } } }
        ] }))
        .unwrap();
        let html = render_puck_page(
            &page,
            &tiles_tera(),
            &registry,
            &ResolvedReferences::default(),
        )
        .unwrap()
        .html;
        assert!(html.contains("a11y error"), "{html}");
        assert!(html.contains("field 'hero'"), "{html}");
    }

    #[test]
    fn references_are_collected_once() {
        let registry = ComponentRegistry::load(&[]).unwrap();
        let page: PuckPage =
            serde_json::from_value(json!({ "content": [ { "type": "Hero", "props": {} } ] }))
                .unwrap();
        assert!(collect_references(&page, &registry).is_empty());
    }

    const PICKS: &str = r#"
type = "Picks"
label = "Picks"
category = "content"
template = "pb/picks.html"

[[fields]]
name = "story"
type = "reference"
target = "item"
item_type = "blog"

[[fields]]
name = "latest"
type = "reference"
target = "gather"
limit = 3
"#;

    #[test]
    fn a_reference_renders_what_it_resolved_to_if_its_type_matches() {
        let dir = std::env::temp_dir().join(format!("trovato-pb-refs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("components")).unwrap();
        std::fs::write(dir.join("components/picks.component.toml"), PICKS).unwrap();
        let registry = ComponentRegistry::load(&[
            crate::content::page_builder_components::PluginComponentSource {
                name: "site".into(),
                dir: dir.clone(),
            },
        ])
        .unwrap();
        std::fs::remove_dir_all(dir).ok();
        let mut tera = engine().tera().clone();
        tera.add_raw_template(
            "pb/picks.html",
            r#"<div>{% if story %}<a href="{{ story.url }}">{{ story.title }}</a>{% endif %}{% if latest %}<p>{{ latest.label }}</p>{% endif %}</div>"#,
        )
        .unwrap();

        let blog = "0190a000-0000-7000-8000-000000000001";
        let page_item = "0190a000-0000-7000-8000-000000000002";
        let page: PuckPage = serde_json::from_value(json!({ "content": [
            { "type": "Picks", "props": { "story": blog, "latest": "recent" } },
            { "type": "Picks", "props": { "story": page_item, "latest": "recent" } }
        ] }))
        .unwrap();
        let keys = collect_references(&page, &registry);
        assert_eq!(keys.len(), 3, "the gather is collected once: {keys:?}");
        assert!(
            keys.iter()
                .any(|k| k.target == ReferenceTarget::Gather && k.limit == 3)
        );

        let mut resolved = ResolvedReferences::default();
        for key in &keys {
            let value = match (key.target, key.id.as_str()) {
                (ReferenceTarget::Gather, _) => json!({ "label": "Recent posts" }),
                (_, id) if id == blog => {
                    json!({ "title": "A blog post", "type": "blog", "url": "/b" })
                }
                _ => json!({ "title": "A plain page", "type": "page", "url": "/p" }),
            };
            resolved.insert(key, value);
        }
        let html = render_puck_page(&page, &tera, &registry, &resolved)
            .unwrap()
            .html;
        assert!(html.contains(">A blog post</a>"), "{html}");
        assert!(!html.contains("A plain page"), "{html}");
        assert_eq!(html.matches("Recent posts").count(), 2, "{html}");
    }
}
