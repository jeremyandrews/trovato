//! Theme engine with Tera templates and suggestion resolution.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use tera::Tera;
use tracing::debug;

use crate::content::FilterPipeline;
use crate::form::Form;
use crate::services::locale::LocaleService;

use super::render::RenderTreeConsumer;

/// CLDR-based localized month names for supported locales.
///
/// Covers 14 languages with genitive forms where applicable (e.g.,
/// Polish uses genitive case for month names in dates: "marca" not "marzec").
/// Unknown locales fall back to English.
fn localized_month_name(month: u32, locale: &str) -> &'static str {
    let primary = locale.split('-').next().unwrap_or(locale);
    let idx = month.saturating_sub(1).min(11) as usize;
    static EN: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    static DE: [&str; 12] = [
        "Januar",
        "Februar",
        "März",
        "April",
        "Mai",
        "Juni",
        "Juli",
        "August",
        "September",
        "Oktober",
        "November",
        "Dezember",
    ];
    static FR: [&str; 12] = [
        "janvier",
        "février",
        "mars",
        "avril",
        "mai",
        "juin",
        "juillet",
        "août",
        "septembre",
        "octobre",
        "novembre",
        "décembre",
    ];
    static ES: [&str; 12] = [
        "enero",
        "febrero",
        "marzo",
        "abril",
        "mayo",
        "junio",
        "julio",
        "agosto",
        "septiembre",
        "octubre",
        "noviembre",
        "diciembre",
    ];
    static IT: [&str; 12] = [
        "gennaio",
        "febbraio",
        "marzo",
        "aprile",
        "maggio",
        "giugno",
        "luglio",
        "agosto",
        "settembre",
        "ottobre",
        "novembre",
        "dicembre",
    ];
    static PT: [&str; 12] = [
        "janeiro",
        "fevereiro",
        "março",
        "abril",
        "maio",
        "junho",
        "julho",
        "agosto",
        "setembro",
        "outubro",
        "novembro",
        "dezembro",
    ];
    static NL: [&str; 12] = [
        "januari",
        "februari",
        "maart",
        "april",
        "mei",
        "juni",
        "juli",
        "augustus",
        "september",
        "oktober",
        "november",
        "december",
    ];
    static PL: [&str; 12] = [
        "stycznia",
        "lutego",
        "marca",
        "kwietnia",
        "maja",
        "czerwca",
        "lipca",
        "sierpnia",
        "września",
        "października",
        "listopada",
        "grudnia",
    ];
    static RU: [&str; 12] = [
        "января",
        "февраля",
        "марта",
        "апреля",
        "мая",
        "июня",
        "июля",
        "августа",
        "сентября",
        "октября",
        "ноября",
        "декабря",
    ];
    static AR: [&str; 12] = [
        "يناير",
        "فبراير",
        "مارس",
        "أبريل",
        "مايو",
        "يونيو",
        "يوليو",
        "أغسطس",
        "سبتمبر",
        "أكتوبر",
        "نوفمبر",
        "ديسمبر",
    ];
    static HE: [&str; 12] = [
        "ינואר",
        "פברואר",
        "מרץ",
        "אפריל",
        "מאי",
        "יוני",
        "יולי",
        "אוגוסט",
        "ספטמבר",
        "אוקטובר",
        "נובמבר",
        "דצמבר",
    ];
    match primary {
        "de" => DE[idx],
        "fr" => FR[idx],
        "es" => ES[idx],
        "it" => IT[idx],
        "pt" => PT[idx],
        "nl" => NL[idx],
        "pl" => PL[idx],
        "ru" => RU[idx],
        "ar" => AR[idx],
        "he" => HE[idx],
        _ => EN[idx],
    }
}

/// Format a date using CLDR locale conventions with correct month names.
///
/// Produces locale-appropriate date strings with localized month names,
/// correct day/month ordering, and locale-specific formatting:
/// - English: "March 30, 2026"
/// - German:  "30. März 2026"
/// - Japanese: "2026年3月30日"
/// - Arabic:  "30 مارس 2026"
fn format_date_localized(dt: &chrono::DateTime<chrono::Utc>, locale: &str) -> String {
    let primary = locale.split('-').next().unwrap_or(locale);
    let day = dt.format("%-d").to_string();
    let month_num = dt.format("%-m").to_string();
    let year = dt.format("%Y").to_string();
    let month_name = localized_month_name(dt.format("%m").to_string().parse().unwrap_or(1), locale);

    match primary {
        "de" => format!("{day}. {month_name} {year}"),
        "fr" | "it" | "nl" | "pl" | "ru" => format!("{day} {month_name} {year}"),
        "es" | "pt" => format!("{day} de {month_name} de {year}"),
        "ja" | "zh" => format!("{year}年{month_num}月{day}日"),
        "ko" => format!("{year}년 {month_num}월 {day}일"),
        "ar" => format!("{day} {month_name} {year}"),
        "he" => format!("{day} ב{month_name} {year}"),
        _ => format!("{month_name} {day}, {year}"),
    }
}
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
    ///
    /// If a `LocaleService` is provided, a `trans` filter is registered that
    /// translates interface strings.
    pub fn new(template_dir: &Path, locale: Option<Arc<LocaleService>>) -> Result<Self> {
        let pattern = template_dir.join("**/*.html");
        let pattern_str = pattern
            .to_str()
            .context("invalid template directory path")?;

        let mut tera = Tera::new(pattern_str).context("failed to initialize Tera templates")?;

        // Register custom filters
        Self::register_filters(&mut tera, locale);

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
    fn register_filters(tera: &mut Tera, locale: Option<Arc<LocaleService>>) {
        // Filter for text format processing
        tera.register_filter(
            "text_format",
            |value: &tera::Value, args: &std::collections::HashMap<String, tera::Value>| {
                let text = tera::try_get_value!("text_format", "value", String, value);
                let format = args
                    .get("format")
                    .and_then(|v| v.as_str())
                    .unwrap_or("plain_text");

                let pipeline = FilterPipeline::for_format_safe(format);
                Ok(tera::Value::String(pipeline.process(&text)))
            },
        );

        // Filter for formatting Unix timestamps as human-readable dates.
        //
        // Usage:
        //   {{ timestamp | format_date }}                     — uses active_language locale
        //   {{ timestamp | format_date(locale="de") }}       — explicit locale
        //   {{ timestamp | format_date(format="%Y-%m-%d") }} — custom format (overrides locale)
        tera.register_filter(
            "format_date",
            |value: &tera::Value, args: &std::collections::HashMap<String, tera::Value>| {
                let timestamp = match value {
                    tera::Value::Number(n) => n.as_i64().unwrap_or(0),
                    _ => return Ok(tera::Value::String(String::new())),
                };

                // Custom format takes precedence over locale
                let custom_format = args.get("format").and_then(|v| v.as_str());

                let formatted = if let Some(fmt) = custom_format {
                    // Custom strftime format — bypasses locale (user's explicit choice)
                    chrono::DateTime::from_timestamp(timestamp, 0)
                        .map(|dt| dt.format(fmt).to_string())
                        .unwrap_or_else(|| "Unknown date".to_string())
                } else {
                    // CLDR-localized formatting with correct month names
                    let locale = args.get("locale").and_then(|v| v.as_str()).unwrap_or("en");
                    chrono::DateTime::from_timestamp(timestamp, 0)
                        .map(|dt| format_date_localized(&dt, locale))
                        .unwrap_or_else(|| "Unknown date".to_string())
                };

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

        // Filter for rendering Blocks fields (JSON array of Editor.js blocks) to HTML.
        // Usage: {{ item.fields.field_description | render_blocks | safe }}
        tera.register_filter(
            "render_blocks",
            |value: &tera::Value, _args: &std::collections::HashMap<String, tera::Value>| {
                if let Some(blocks) = value.as_array() {
                    Ok(tera::Value::String(crate::content::render_blocks(blocks)))
                } else if let Some(s) = value.as_str() {
                    // Plain string fallback — return as-is (handles TextLong values)
                    Ok(tera::Value::String(s.to_string()))
                } else {
                    Ok(tera::Value::String(String::new()))
                }
            },
        );

        // Filter for displaying FieldType enum variants as human-readable labels.
        // FieldType serializes as either a string ("Date", "Boolean", "Blocks") or
        // an object ({"Text": {"max_length": null}}). This filter extracts the
        // variant name for display in admin templates.
        // Usage: {{ field.field_type | field_type_label }}
        tera.register_filter(
            "field_type_label",
            |value: &tera::Value, _args: &std::collections::HashMap<String, tera::Value>| {
                if let Some(s) = value.as_str() {
                    // Simple variant: "Date", "Boolean", "TextLong", "Blocks", etc.
                    Ok(tera::Value::String(s.to_string()))
                } else if let Some(obj) = value.as_object() {
                    // Object variant: {"Text": {...}}, {"Compound": {...}}, etc.
                    if let Some(key) = obj.keys().next() {
                        Ok(tera::Value::String(key.clone()))
                    } else {
                        Ok(tera::Value::String("Unknown".to_string()))
                    }
                } else {
                    Ok(tera::Value::String("Unknown".to_string()))
                }
            },
        );

        // Filter for translating interface strings via LocaleService.
        // Usage: {{ "Subscribe" | trans(lang=active_language) }}
        if let Some(locale_service) = locale {
            tera.register_filter(
                "trans",
                move |value: &tera::Value,
                      args: &std::collections::HashMap<String, tera::Value>| {
                    let source = tera::try_get_value!("trans", "value", String, value);
                    let lang = args.get("lang").and_then(|v| v.as_str()).unwrap_or("en");
                    let context = args.get("context").and_then(|v| v.as_str()).unwrap_or("");
                    Ok(tera::Value::String(
                        locale_service.translate(&source, context, lang),
                    ))
                },
            );
        }
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
        self.render_form_with_errors(form, &[])
    }

    /// Render a form to HTML with validation errors.
    ///
    /// Field-level errors (those with `field: Some(name)`) are passed to
    /// individual element templates as `field_error`, enabling `aria-describedby`
    /// and `aria-invalid` attributes for accessibility.
    pub fn render_form_with_errors(
        &self,
        form: &Form,
        errors: &[crate::form::ValidationError],
    ) -> Result<String> {
        use std::collections::HashMap;

        let mut context = tera::Context::new();
        context.insert("form", form);
        context.insert("errors", errors);

        // Build field→error lookup for per-element error rendering
        let field_errors: HashMap<&str, &str> = errors
            .iter()
            .filter_map(|e| e.field.as_deref().map(|f| (f, e.message.as_str())))
            .collect();

        // Render form elements with field errors
        let elements_html =
            self.render_form_elements_with_errors(form, &field_errors, &mut context)?;
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
        let empty = std::collections::HashMap::new();
        self.render_form_elements_with_errors(form, &empty, context)
    }

    /// Render form elements to HTML, passing per-field errors to each element template.
    fn render_form_elements_with_errors(
        &self,
        form: &Form,
        field_errors: &std::collections::HashMap<&str, &str>,
        context: &mut tera::Context,
    ) -> Result<String> {
        use std::fmt::Write;
        let mut html = String::new();

        // Sort elements by weight
        let mut elements: Vec<_> = form.elements.iter().collect();
        elements.sort_by_key(|(_, el)| el.weight);

        for (name, element) in elements {
            let field_error = field_errors.get(name.as_str()).copied();
            let element_html = self.render_form_element(name, element, field_error, context)?;
            // Infallible: write!() to String is infallible
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
        field_error: Option<&str>,
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

        // Pass per-field error for aria-describedby and aria-invalid
        if let Some(error_msg) = field_error {
            el_context.insert("field_error", error_msg);
        }

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
            // Children don't receive per-field errors (errors are on top-level fields)
            let child_html = self.render_form_element(name, child, None, context)?;
            // Infallible: write!() to String is infallible
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
// Tests are allowed to use unwrap/expect freely.
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
            current_revision_id: None,
            stage_id: trovato_sdk::types::live_stage_id(),
            created: 0,
            changed: 0,
            language: None,
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
        ThemeEngine::register_filters(&mut tera, None);

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
        ThemeEngine::register_filters(&mut tera, None);

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
        ThemeEngine::register_filters(&mut tera, None);

        tera.add_raw_template("test", "{{ ts | format_date }}")
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("ts", "not a number");
        let result = tera.render("test", &ctx).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_format_date_german_locale() {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera, None);

        tera.add_raw_template("test", r#"{{ ts | format_date(locale="de") }}"#)
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("ts", &1739577600_i64); // 2025-02-15 00:00:00 UTC
        let result = tera.render("test", &ctx).unwrap();
        // German CLDR format with localized month name
        assert_eq!(result, "15. Februar 2025", "German format: {result}");
    }

    #[test]
    fn test_format_date_japanese_locale() {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera, None);

        tera.add_raw_template("test", r#"{{ ts | format_date(locale="ja") }}"#)
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("ts", &1739577600_i64);
        let result = tera.render("test", &ctx).unwrap();
        // Japanese CLDR format
        assert_eq!(result, "2025年2月15日", "Japanese format: {result}");
    }

    #[test]
    fn test_format_date_localized_month_names() {
        // Verify all locales produce correct month names (not English)
        let ts = 1711756800_i64; // 2024-03-30 00:00:00 UTC
        let dt = chrono::DateTime::from_timestamp(ts, 0).unwrap();

        assert_eq!(format_date_localized(&dt, "de"), "30. März 2024");
        assert_eq!(format_date_localized(&dt, "fr"), "30 mars 2024");
        assert_eq!(format_date_localized(&dt, "es"), "30 de marzo de 2024");
        assert_eq!(format_date_localized(&dt, "it"), "30 marzo 2024");
        assert_eq!(format_date_localized(&dt, "ja"), "2024年3月30日");
        assert_eq!(format_date_localized(&dt, "ko"), "2024년 3월 30일");
        assert_eq!(format_date_localized(&dt, "pl"), "30 marca 2024");
        assert_eq!(format_date_localized(&dt, "en"), "March 30, 2024");
    }

    #[test]
    fn test_format_date_custom_format_overrides_locale() {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera, None);

        tera.add_raw_template(
            "test",
            r#"{{ ts | format_date(locale="de", format="%Y-%m-%d") }}"#,
        )
        .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("ts", &1739577600_i64);
        let result = tera.render("test", &ctx).unwrap();
        assert_eq!(result, "2025-02-15");
    }
}
