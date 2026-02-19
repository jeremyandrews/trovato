//! Theme engine with Tera templates and suggestion resolution.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use tera::Tera;
use tracing::debug;

use crate::content::FilterPipeline;
use crate::form::Form;

use super::render::RenderTreeConsumer;
use trovato_sdk::render::RenderElement;
use trovato_sdk::types::Item;

/// Theme engine for rendering templates.
pub struct ThemeEngine {
    /// Tera template engine instance.
    tera: Tera,
    /// Cache mapping suggestion lists to resolved template names.
    suggestion_cache: DashMap<String, String>,
    /// Render tree consumer for RenderElement → HTML.
    render_consumer: RenderTreeConsumer,
}

impl ThemeEngine {
    /// Create a new theme engine loading templates from the given directory.
    pub fn new(template_dir: &Path) -> Result<Self> {
        let pattern = template_dir.join("**/*.html");
        let pattern_str = pattern
            .to_str()
            .context("invalid template directory path")?;

        let mut tera = Tera::new(pattern_str).context("failed to initialize Tera templates")?;

        // Register custom filters
        Self::register_filters(&mut tera);

        let template_names: Vec<_> = tera.get_template_names().collect();
        debug!(count = template_names.len(), "loaded templates");

        Ok(Self {
            tera,
            suggestion_cache: DashMap::new(),
            render_consumer: RenderTreeConsumer::new(),
        })
    }

    /// Create a theme engine with no templates (for testing).
    pub fn empty() -> Result<Self> {
        let tera = Tera::default();
        Ok(Self {
            tera,
            suggestion_cache: DashMap::new(),
            render_consumer: RenderTreeConsumer::new(),
        })
    }

    /// Register custom Tera filters.
    fn register_filters(tera: &mut Tera) {
        // Filter for text format processing
        tera.register_filter(
            "text_format",
            |value: &tera::Value, args: &std::collections::HashMap<String, tera::Value>| {
                let text = tera::try_get_value!("text_format", "value", String, value);
                let format = args
                    .get("format")
                    .and_then(|v| v.as_str())
                    .unwrap_or("plain_text");

                let pipeline = FilterPipeline::for_format(format);
                Ok(tera::Value::String(pipeline.process(&text)))
            },
        );

        // Filter for formatting Unix timestamps as human-readable dates
        tera.register_filter(
            "format_date",
            |value: &tera::Value, _args: &std::collections::HashMap<String, tera::Value>| {
                let timestamp = match value {
                    tera::Value::Number(n) => n.as_i64().unwrap_or(0),
                    _ => return Ok(tera::Value::String(String::new())),
                };

                let formatted = chrono::DateTime::from_timestamp(timestamp, 0)
                    .map(|dt| dt.format("%B %-d, %Y").to_string())
                    .unwrap_or_else(|| "Unknown date".to_string());

                Ok(tera::Value::String(formatted))
            },
        );

        // Filter for safe HTML output (already filtered)
        tera.register_filter(
            "safe_html",
            |value: &tera::Value, _args: &std::collections::HashMap<String, tera::Value>| {
                // Mark as safe by returning as-is (templates should use |safe after this)
                Ok(value.clone())
            },
        );
    }

    /// Get the underlying Tera instance for custom operations.
    pub fn tera(&self) -> &Tera {
        &self.tera
    }

    /// Get a mutable reference to Tera (for adding templates at runtime).
    pub fn tera_mut(&mut self) -> &mut Tera {
        &mut self.tera
    }

    /// Resolve the best template from a list of suggestions.
    ///
    /// Templates are tried in order; the first one that exists is returned.
    /// Results are cached for performance.
    ///
    /// Example suggestions: `["item--blog--123", "item--blog", "item"]`
    pub fn resolve_template(&self, suggestions: &[&str]) -> Option<String> {
        if suggestions.is_empty() {
            return None;
        }

        // Build cache key from suggestions
        let cache_key = suggestions.join("|");

        // Check cache first
        if let Some(cached) = self.suggestion_cache.get(&cache_key) {
            return Some(cached.clone());
        }

        // Find first template that exists
        for suggestion in suggestions {
            let template_name = format!("{suggestion}.html");
            if self.tera.get_template(&template_name).is_ok() {
                self.suggestion_cache
                    .insert(cache_key, template_name.clone());
                return Some(template_name);
            }

            // Also try without .html extension (in case suggestion already has it)
            if self.tera.get_template(suggestion).is_ok() {
                let name = (*suggestion).to_string();
                self.suggestion_cache.insert(cache_key, name.clone());
                return Some(name);
            }
        }

        // Cache miss - no template found
        // Don't cache negative results to allow hot-reload
        None
    }

    /// Generate template suggestions for an item.
    ///
    /// Returns suggestions from most specific to least:
    /// - `item--{type}--{id}`
    /// - `item--{type}`
    /// - `item`
    pub fn item_suggestions(item: &Item) -> Vec<String> {
        let type_name = &item.item_type;
        let id = item.id;

        vec![
            format!("elements/item--{}--{}", type_name, id),
            format!("elements/item--{}", type_name),
            "elements/item".to_string(),
        ]
    }

    /// Render a RenderElement tree to HTML.
    pub fn render_element(
        &self,
        element: &RenderElement,
        context: &mut tera::Context,
    ) -> Result<String> {
        self.render_consumer.render(&self.tera, element, context)
    }

    /// Render an item using template suggestions.
    pub fn render_item(&self, item: &Item, element: &RenderElement) -> Result<String> {
        let suggestions = Self::item_suggestions(item);
        let suggestion_refs: Vec<&str> = suggestions.iter().map(|s| s.as_str()).collect();

        let template = self
            .resolve_template(&suggestion_refs)
            .unwrap_or_else(|| "elements/item.html".to_string());

        let mut context = tera::Context::new();
        context.insert("item", item);
        context.insert("element", element);

        // Render children first, then the wrapper template
        let children_html = self.render_element(element, &mut context)?;
        context.insert("children", &children_html);

        self.tera
            .render(&template, &context)
            .context("failed to render item template")
    }

    /// Render a form to HTML.
    pub fn render_form(&self, form: &Form) -> Result<String> {
        let mut context = tera::Context::new();
        context.insert("form", form);

        // Render form elements
        let elements_html = self.render_form_elements(form, &mut context)?;
        context.insert("elements", &elements_html);

        let template = self
            .resolve_template(&["form/form"])
            .unwrap_or_else(|| "form/form.html".to_string());

        self.tera
            .render(&template, &context)
            .context("failed to render form template")
    }

    /// Render form elements to HTML.
    fn render_form_elements(&self, form: &Form, context: &mut tera::Context) -> Result<String> {
        use std::fmt::Write;
        let mut html = String::new();

        // Sort elements by weight
        let mut elements: Vec<_> = form.elements.iter().collect();
        elements.sort_by_key(|(_, el)| el.weight);

        for (name, element) in elements {
            let element_html = self.render_form_element(name, element, context)?;
            // SAFETY: write!() to String is infallible
            #[allow(clippy::unwrap_used)]
            write!(html, "{element_html}").unwrap();
        }

        Ok(html)
    }

    /// Render a single form element to HTML.
    fn render_form_element(
        &self,
        name: &str,
        element: &crate::form::FormElement,
        context: &mut tera::Context,
    ) -> Result<String> {
        use crate::form::ElementType;

        let template_name = match &element.element_type {
            ElementType::Textfield { .. } => "form/textfield.html",
            ElementType::Textarea { .. } => "form/textarea.html",
            ElementType::Select { .. } => "form/select.html",
            ElementType::Checkbox => "form/checkbox.html",
            ElementType::Checkboxes { .. } => "form/checkboxes.html",
            ElementType::Radio { .. } => "form/radio.html",
            ElementType::Hidden => "form/hidden.html",
            ElementType::Password => "form/password.html",
            ElementType::File => "form/file.html",
            ElementType::Submit { .. } => "form/submit.html",
            ElementType::Fieldset { .. } => "form/fieldset.html",
            ElementType::Markup { .. } => "form/markup.html",
            ElementType::Container => "form/container.html",
        };

        // Build element context
        let mut el_context = context.clone();
        el_context.insert("name", name);
        el_context.insert("element", element);

        // Render children for container types
        if !element.children.is_empty() {
            let children_html = self.render_form_children(element, context)?;
            el_context.insert("children", &children_html);
        }

        self.tera
            .render(template_name, &el_context)
            .with_context(|| format!("failed to render form element: {name}"))
    }

    /// Render form element children.
    fn render_form_children(
        &self,
        element: &crate::form::FormElement,
        context: &mut tera::Context,
    ) -> Result<String> {
        use std::fmt::Write;
        let mut html = String::new();

        let mut children: Vec<_> = element.children.iter().collect();
        children.sort_by_key(|(_, el)| el.weight);

        for (name, child) in children {
            let child_html = self.render_form_element(name, child, context)?;
            // SAFETY: write!() to String is infallible
            #[allow(clippy::unwrap_used)]
            write!(html, "{child_html}").unwrap();
        }

        Ok(html)
    }

    /// Check if a path is an admin path.
    ///
    /// Admin paths use a different template set (page--admin.html vs page.html).
    pub fn is_admin_path(path: &str) -> bool {
        path.starts_with("/admin")
    }

    /// Get page template suggestions based on path.
    pub fn page_suggestions(path: &str) -> Vec<String> {
        let mut suggestions = Vec::new();

        // Convert path to template suggestion
        // /admin/structure/types -> page--admin--structure--types
        let normalized = path.trim_start_matches('/').replace('/', "--");
        if !normalized.is_empty() {
            suggestions.push(format!("page--{normalized}"));
        }

        // Add admin base if it's an admin path
        if Self::is_admin_path(path) {
            suggestions.push("page--admin".to_string());
        }

        suggestions.push("page".to_string());

        suggestions
    }

    /// Render a full page with content.
    pub fn render_page(
        &self,
        path: &str,
        title: &str,
        content: &str,
        context: &mut tera::Context,
    ) -> Result<String> {
        let suggestions = Self::page_suggestions(path);
        let suggestion_refs: Vec<&str> = suggestions.iter().map(|s| s.as_str()).collect();

        let template = self
            .resolve_template(&suggestion_refs)
            .unwrap_or_else(|| "page.html".to_string());

        context.insert("title", title);
        context.insert("content", content);
        context.insert("path", path);
        context.insert("is_admin", &Self::is_admin_path(path));

        self.tera
            .render(&template, context)
            .context("failed to render page template")
    }

    /// Clear the suggestion cache (useful for development hot-reload).
    pub fn clear_cache(&self) {
        self.suggestion_cache.clear();
    }

    /// Reload templates from disk.
    pub fn reload(&mut self) -> Result<()> {
        self.tera
            .full_reload()
            .context("failed to reload templates")?;
        self.clear_cache();
        Ok(())
    }
}

impl std::fmt::Debug for ThemeEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThemeEngine")
            .field("template_count", &self.tera.get_template_names().count())
            .field("cache_size", &self.suggestion_cache.len())
            .finish()
    }
}

/// Wrap ThemeEngine in Arc for sharing across handlers.
pub type SharedThemeEngine = Arc<ThemeEngine>;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_is_admin_path() {
        assert!(ThemeEngine::is_admin_path("/admin"));
        assert!(ThemeEngine::is_admin_path("/admin/structure/types"));
        assert!(!ThemeEngine::is_admin_path("/item/123"));
        assert!(!ThemeEngine::is_admin_path("/"));
    }

    #[test]
    fn test_page_suggestions() {
        let suggestions = ThemeEngine::page_suggestions("/admin/structure/types");
        assert_eq!(
            suggestions,
            vec!["page--admin--structure--types", "page--admin", "page"]
        );

        let suggestions = ThemeEngine::page_suggestions("/item/123");
        assert_eq!(suggestions, vec!["page--item--123", "page"]);
    }

    #[test]
    fn test_item_suggestions() {
        use std::collections::HashMap;
        use uuid::Uuid;

        let item = Item {
            id: Uuid::nil(),
            item_type: "blog".to_string(),
            title: "Test".to_string(),
            fields: HashMap::new(),
            status: 1,
            author_id: Uuid::nil(),
            revision_id: None,
            stage_id: None,
            created: 0,
            changed: 0,
        };

        let suggestions = ThemeEngine::item_suggestions(&item);
        assert_eq!(suggestions.len(), 3);
        assert!(suggestions[0].contains("item--blog--"));
        assert_eq!(suggestions[1], "elements/item--blog");
        assert_eq!(suggestions[2], "elements/item");
    }

    #[test]
    fn test_format_date_filter_with_valid_timestamp() {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera);

        tera.add_raw_template("test", "{{ ts | format_date }}")
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("ts", &1739577600_i64); // 2025-02-15 00:00:00 UTC
        let result = tera.render("test", &ctx).unwrap();
        assert_eq!(result, "February 15, 2025");
    }

    #[test]
    fn test_format_date_filter_with_zero() {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera);

        tera.add_raw_template("test", "{{ ts | format_date }}")
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("ts", &0_i64);
        let result = tera.render("test", &ctx).unwrap();
        assert_eq!(result, "January 1, 1970");
    }

    #[test]
    fn test_format_date_filter_with_string() {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera);

        tera.add_raw_template("test", "{{ ts | format_date }}")
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("ts", "not a number");
        let result = tera.render("test", &ctx).unwrap();
        assert_eq!(result, "");
    }
}
