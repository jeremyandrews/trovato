//! Theme engine with Tera templates and suggestion resolution.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use anyhow::{Context, Result};
use dashmap::DashMap;
use tera::Tera;
use tracing::debug;

use crate::content::FilterPipeline;
use crate::form::Form;
use crate::metrics::{CacheName, Metrics};
use crate::services::locale::LocaleService;

use super::render::RenderTreeConsumer;

/// The `text_format` Tera filter: `{{ value | text_format(format=fmt) }}`.
///
/// A struct rather than a closure so it can report [`tera::Filter::is_safe`]
/// as `true`: every pipeline [`FilterPipeline::for_format`] builds, including
/// `full_html`'s, is sanitized before it is returned, so Tera does not need to
/// HTML-escape the result and a template does not need `| safe`.
struct TextFormatFilter;

impl tera::Filter for TextFormatFilter {
    fn filter(
        &self,
        value: &tera::Value,
        args: &std::collections::HashMap<String, tera::Value>,
    ) -> tera::Result<tera::Value> {
        let text = tera::try_get_value!("text_format", "value", String, value);
        let format = args
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("plain_text");
        let pipeline = FilterPipeline::for_format(format);
        Ok(tera::Value::String(pipeline.process(&text)))
    }

    fn is_safe(&self) -> bool {
        true
    }
}

tokio::task_local! {
    /// Whether the request being handled is for an `/admin` path, set by
    /// [`scope_request`]; outside a request (a background task) it is unset,
    /// and the default theme renders.
    static ADMIN_REQUEST: bool;
}

/// Run `f`, a request for `path`, with the theme chosen for that path: the
/// admin theme for `/admin`, the default theme otherwise.
pub async fn scope_request<F: std::future::Future>(path: &str, f: F) -> F::Output {
    ADMIN_REQUEST
        .scope(ThemeEngine::is_admin_path(path), f)
        .await
}

/// The themes in use: their machine names, `stock` for the stock theme.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ActiveThemes {
    /// Every page outside `/admin`.
    pub default: String,
    /// `/admin` pages.
    pub admin: String,
}

impl Default for ActiveThemes {
    fn default() -> Self {
        Self {
            default: super::registry::STOCK.to_string(),
            admin: super::registry::STOCK.to_string(),
        }
    }
}

/// Class values kept on a `<span>` inside sanitized Markdown.
///
/// The token classes Prism and highlight.js apply, and nothing else. They matter
/// only for HTML that arrives already highlighted — a client-side highlighter
/// adds its spans in the browser, long after this runs, so it never meets the
/// sanitizer at all. What that highlighter *does* need is the `language-` class
/// on the block, which is what [`is_allowed_class`] keeps.
const HIGHLIGHT_TOKEN_CLASSES: [&str; 34] = [
    // highlight.js
    "hljs",
    // Prism's marker class, applied alongside a token type below.
    "token",
    // Token types, shared in spelling by both highlighters.
    "atrule",
    "attr-name",
    "attr-value",
    "boolean",
    "builtin",
    "cdata",
    "class-name",
    "comment",
    "constant",
    "deleted",
    "doctype",
    "entity",
    "function",
    "important",
    "inserted",
    "keyword",
    "namespace",
    "number",
    "operator",
    "prolog",
    "property",
    "punctuation",
    "regex",
    "selector",
    "string",
    "symbol",
    "tag",
    "url",
    "variable",
    // Emphasis a highlighter applies to a token; harmless, and dropping them
    // makes highlighted output look wrong for no gain.
    "bold",
    "italic",
    "highlighted",
];

/// Whether one class value survives sanitization on `element`.
///
/// `language-rust` and `lang-rust` on the block, a highlighter's token class on a
/// span, and nothing else. A language name is bounded to the characters real ones
/// use, so `language-` cannot smuggle an arbitrary value into the attribute.
fn is_allowed_class(element: &str, class: &str) -> bool {
    if let Some(language) = class
        .strip_prefix("language-")
        .or_else(|| class.strip_prefix("lang-"))
    {
        return matches!(element, "code" | "pre")
            && !language.is_empty()
            && language.len() <= 32
            && language
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '#' | '.'));
    }

    // highlight.js namespaces its own token classes; Prism does not.
    if let Some(rest) = class.strip_prefix("hljs-") {
        return element == "span"
            && !rest.is_empty()
            && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    }

    element == "span" && HIGHLIGHT_TOKEN_CLASSES.contains(&class)
}

/// The sanitizer the `markdown` filter runs its output through.
///
/// `ammonia::clean`'s defaults strip every `class`, which is what made a
/// highlighted code block impossible: `pulldown_cmark` renders a fenced
/// ```` ```rust ```` block as `<code class="language-rust">`, the class was
/// removed, and no highlighter — server-side or in the browser — could then tell
/// what language the block was.
///
/// `class` is allowed back on `code`, `pre` and `span` only, and only for the
/// values [`is_allowed_class`] accepts. Everything else is unchanged from
/// ammonia's defaults, so a `class` on a `div`, a `style` attribute, an `onclick`
/// and a `<script>` are all still removed.
static MARKDOWN_SANITIZER: LazyLock<ammonia::Builder<'static>> = LazyLock::new(|| {
    let mut builder = ammonia::Builder::default();
    builder.add_tag_attributes("code", ["class"]);
    builder.add_tag_attributes("pre", ["class"]);
    builder.add_tag_attributes("span", ["class"]);
    builder.attribute_filter(|element, attribute, value| {
        if attribute != "class" {
            return Some(value.into());
        }
        let kept: Vec<&str> = value
            .split_whitespace()
            .filter(|class| is_allowed_class(element, class))
            .collect();
        // No surviving class means no attribute, rather than `class=""`.
        if kept.is_empty() {
            None
        } else {
            Some(kept.join(" ").into())
        }
    });
    builder
});

/// Render Markdown to sanitized HTML, rewriting headings so none is above
/// `min_heading` (1 leaves them alone).
///
/// The `markdown` Tera filter and the page builder's rich text fields both
/// come through here, so one sanitizer decides what Markdown may produce:
/// raw HTML in the source is cleaned, a code block keeps the class it needs to
/// be highlighted.
pub fn render_markdown(text: &str, min_heading: usize) -> String {
    let processed = if min_heading > 1 {
        crate::content::page_builder::rewrite_markdown_headings(text, min_heading)
    } else {
        text.to_string()
    };

    let parser = pulldown_cmark::Parser::new(&processed);
    let mut html_output = String::new();
    pulldown_cmark::html::push_html(&mut html_output, parser);

    // Sanitize to prevent XSS from raw HTML in Markdown, keeping the
    // class attribute a code block needs to be highlightable.
    MARKDOWN_SANITIZER.clean(&html_output).to_string()
}

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

/// One template file that does not parse (B138).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenTemplate {
    /// The file on disk.
    pub path: PathBuf,
    /// Tera's parse message, with the line and column it points at.
    pub message: String,
}

/// Every template on a search path that does not parse (B138).
///
/// Building the engine used to fail with "failed to load templates from the
/// search path" and nothing else in the error's own message, so a log line
/// formatted with `%` did not say which file was at fault. This names every
/// broken file, by path, with Tera's message, so an operator fixes them all in
/// one pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateParseError {
    pub broken: Vec<BrokenTemplate>,
}

impl TemplateParseError {
    /// The same report with each path shown relative to `base` where it lies
    /// under it: what the appearance screen shows for a theme, so a theme's
    /// error names `templates/base.html` and not the server's directory tree.
    pub fn relative_to(&self, base: &Path) -> String {
        self.describe(|path| {
            path.strip_prefix(base)
                .unwrap_or(path)
                .display()
                .to_string()
        })
    }

    fn describe(&self, show: impl Fn(&Path) -> String) -> String {
        match self.broken.as_slice() {
            [one] => format!(
                "template {} does not parse: {}",
                show(&one.path),
                one.message
            ),
            many => {
                let mut out = format!("{} templates do not parse:", many.len());
                for b in many {
                    out.push_str(&format!("\n{}: {}", show(&b.path), b.message));
                }
                out
            }
        }
    }
}

impl std::fmt::Display for TemplateParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe(|path| path.display().to_string()))
    }
}

impl std::error::Error for TemplateParseError {}

/// A Tera error and every error under it, as one string: Tera puts the
/// parse message with its line in a source error, under a generic one.
fn tera_error_text(e: &tera::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(inner) = source {
        text.push_str(": ");
        text.push_str(&inner.to_string());
        source = inner.source();
    }
    text.trim().to_string()
}

/// Parse each file on its own, before Tera loads them together, so a syntax
/// error is reported against its file and every broken file is found, not
/// only the first. Only the templates that will be loaded are checked: one a
/// later root replaces is never parsed, as before.
fn check_templates_parse(files: &[(PathBuf, Option<String>)]) -> Result<(), TemplateParseError> {
    let mut broken = Vec::new();
    for (path, name) in files {
        let name = name.as_deref().unwrap_or_default();
        let message = match std::fs::read_to_string(path) {
            Ok(source) => {
                match tera::Template::new(name, Some(path.display().to_string()), &source) {
                    Ok(_) => continue,
                    Err(e) => tera_error_text(&e),
                }
            }
            Err(e) => format!("cannot be read: {e}"),
        };
        broken.push(BrokenTemplate {
            path: path.clone(),
            message,
        });
    }
    if broken.is_empty() {
        Ok(())
    } else {
        Err(TemplateParseError { broken })
    }
}

/// Theme engine for rendering templates.
pub struct ThemeEngine {
    /// Tera template engine instance: the stock theme.
    tera: Tera,
    /// The deployment's themes, each the stock search path with the theme's
    /// templates laid over it (`theme::registry`).
    themes: Vec<(super::registry::ThemeInfo, Tera)>,
    /// The deployment's themes whose templates did not load, each with the
    /// reason, paths relative to the theme directory (B138). Listed on the
    /// appearance screen as unusable; never active.
    refused: Vec<(super::registry::ThemeInfo, String)>,
    /// The default and admin themes in use.
    active: std::sync::RwLock<ActiveThemes>,
    /// Cache mapping suggestion lists to resolved template names.
    suggestion_cache: DashMap<String, String>,
    /// Counts `suggestion_cache` hits and misses.
    metrics: Arc<Metrics>,
    /// Render tree consumer for RenderElement → HTML.
    render_consumer: RenderTreeConsumer,
    /// The page builder components pages may use: the standard set and the
    /// enabled plugins'. The standard set until [`Self::with_components`].
    components: Arc<crate::content::page_builder_components::ComponentRegistry>,
}

impl ThemeEngine {
    /// Create a new theme engine loading templates from a search path.
    ///
    /// Directories are applied in order and a later one overrides a template of
    /// the same name, so an application can ship its own templates from its own
    /// repository, and customise a kernel template, without editing the kernel
    /// tree in place. A single-element slice reproduces the old behaviour
    /// exactly.
    ///
    /// If a `LocaleService` is provided, a `trans` filter is registered that
    /// translates interface strings.
    fn build_tera(template_dirs: &[PathBuf], locale: Option<Arc<LocaleService>>) -> Result<Tera> {
        if template_dirs.is_empty() {
            anyhow::bail!("at least one template directory is required");
        }

        // Every directory is collected into ONE Tera instance rather than one
        // instance per directory that is then extended. Inheritance is the
        // reason: an app template that says
        //   {% extends "gather/query.html" %}
        // has to resolve against the kernel's base template, which lives in a
        // different root. Building a Tera per directory makes that lookup fail
        // at construction time, and the whole engine falls back to empty, so
        // the site silently renders untemplated. One instance, names resolved
        // across all roots, is what makes an overlay usable at all.
        let mut by_name: std::collections::HashMap<String, PathBuf> =
            std::collections::HashMap::new();
        let mut order: Vec<String> = Vec::new();

        for dir in template_dirs {
            for (path, name) in Self::collect_templates(dir) {
                // A later root replaces an earlier template of the same name,
                // which is how an app overrides a kernel template.
                if by_name.insert(name.clone(), path).is_none() {
                    order.push(name);
                }
            }
        }

        let files: Vec<(PathBuf, Option<String>)> = order
            .into_iter()
            .filter_map(|name| by_name.get(&name).map(|p| (p.clone(), Some(name))))
            .collect();

        // A syntax error is named with its file and Tera's message (B138).
        check_templates_parse(&files)?;

        let mut tera = Tera::default();
        tera.add_template_files(files).map_err(|e| {
            // Every file parsed, so this is how they fit together: an
            // `extends` or macro import naming a template that is not loaded.
            anyhow::anyhow!(
                "templates on the search path do not load together: {}",
                tera_error_text(&e)
            )
        })?;

        // Register custom filters
        Self::register_filters(&mut tera, locale);

        let template_names: Vec<_> = tera.get_template_names().collect();
        debug!(count = template_names.len(), "loaded templates");
        Ok(tera)
    }

    /// Create a new theme engine loading templates from a search path.
    ///
    /// Directories are applied in order and a later one overrides a template of
    /// the same name. The result is the stock theme; [`Self::with_themes`] adds
    /// the deployment's.
    pub fn new(
        template_dirs: &[PathBuf],
        locale: Option<Arc<LocaleService>>,
        metrics: Arc<Metrics>,
    ) -> Result<Self> {
        let tera = Self::build_tera(template_dirs, locale)?;
        Ok(Self {
            tera,
            themes: Vec::new(),
            refused: Vec::new(),
            active: std::sync::RwLock::new(ActiveThemes::default()),
            suggestion_cache: DashMap::new(),
            metrics,
            render_consumer: RenderTreeConsumer::new(),
            components: Arc::new(Default::default()),
        })
    }

    /// Collect every template under `root` as `(absolute_path, tera_name)`.
    ///
    /// The name is the path relative to `root` with forward slashes, which is
    /// exactly the name `Tera::new`'s glob would have produced, so a
    /// single-directory search path loads under identical names to before.
    ///
    /// A missing directory yields nothing rather than an error, which keeps an
    /// optional app overlay directory from being a hard startup failure.
    fn collect_templates(root: &Path) -> Vec<(PathBuf, String)> {
        fn walk(dir: &Path, root: &Path, out: &mut Vec<(PathBuf, String)>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, root, out);
                } else if path
                    .extension()
                    .and_then(|e| e.to_str())
                    // .txt covers the plain-text email templates.
                    .is_some_and(|e| e == "html" || e == "txt")
                    && let Ok(rel) = path.strip_prefix(root)
                {
                    let name = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/");
                    out.push((path.clone(), name));
                }
            }
        }

        let mut out = Vec::new();
        walk(root, root, &mut out);
        // read_dir order is filesystem-dependent; sort so template load order
        // (and therefore any name collision within one root) is reproducible.
        out.sort();
        out
    }

    /// Create a theme engine with no templates, for tests only. The server never
    /// falls back to it: a template that does not parse stops startup (B138).
    pub fn empty(metrics: Arc<Metrics>) -> Result<Self> {
        let tera = Tera::default();
        Ok(Self {
            tera,
            themes: Vec::new(),
            refused: Vec::new(),
            active: std::sync::RwLock::new(ActiveThemes::default()),
            suggestion_cache: DashMap::new(),
            metrics,
            render_consumer: RenderTreeConsumer::new(),
            components: Arc::new(Default::default()),
        })
    }

    /// Register custom Tera filters.
    fn register_filters(tera: &mut Tera, locale: Option<Arc<LocaleService>>) {
        // `asset_version()`: the string a template appends to a kernel
        // stylesheet's URL (`?v=...`), so a release that changes the file is
        // fetched afresh rather than served from a day-old cache. The kernel
        // version, because a kernel stylesheet changes only with the kernel.
        tera.register_function(
            "asset_version",
            |_: &std::collections::HashMap<String, tera::Value>| {
                Ok(tera::Value::String(
                    crate::update_status::running_version().to_string(),
                ))
            },
        );

        // Filter for text format processing: a struct, not a closure, so it
        // can report `is_safe() == true` (its output is sanitized by
        // construction — see `TextFormatFilter`) — needs no `| safe`.
        tera.register_filter("text_format", TextFormatFilter);

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

        // The text of stored markup, for a teaser or summary: tags stripped,
        // character references decoded, whitespace collapsed. The result is
        // plain text, so autoescape escapes it exactly once; printing a stored
        // HTML body through `truncate` alone showed its tags, and a stored
        // `&amp;` as `&amp;` (B123).
        // Usage: {{ body.value | plain_text | truncate(length=200) }}
        tera.register_filter(
            "plain_text",
            |value: &tera::Value, _args: &std::collections::HashMap<String, tera::Value>| {
                Ok(tera::Value::String(
                    value
                        .as_str()
                        .map(crate::content::page_meta::plain_text)
                        .unwrap_or_default(),
                ))
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
        //
        // The output goes out under `| safe`, so everything it returns must
        // already be safe. Blocks are structured and `block_render` sanitizes
        // them. A string is not blocks and is not markup: whatever wrote it (a
        // plugin's raw SQL, a config import, a TextLong value) never passed
        // through a sanitizer, so it is escaped and renders as text. Content
        // that is meant to be markup belongs to the `markdown` or `text_format`
        // filter, not this one.
        tera.register_filter(
            "render_blocks",
            |value: &tera::Value, _args: &std::collections::HashMap<String, tera::Value>| {
                if let Some(blocks) = value.as_array() {
                    Ok(tera::Value::String(crate::content::render_blocks(blocks)))
                } else if let Some(s) = value.as_str() {
                    Ok(tera::Value::String(crate::routes::helpers::html_escape(s)))
                } else {
                    Ok(tera::Value::String(String::new()))
                }
            },
        );

        // Filter for rendering Markdown text to sanitized HTML.
        // Usage:
        //   {{ body | markdown | safe }}                — default, no heading rewrite
        //   {{ body | markdown(min_heading=2) | safe }} — rewrite # to ##
        //
        // The `min_heading` parameter rewrites Markdown headings so the minimum
        // level is the specified value. Used by the page builder's TextBlock to
        // prevent rogue H1 tags (page template owns H1).
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

                Ok(tera::Value::String(render_markdown(text, min_heading)))
            },
        );

        // Filter for generating responsive <picture> elements with AVIF/WebP/JPEG.
        //
        // Usage:
        //   {{ "/uploads/photo.jpg" | responsive_image(alt="Photo", widths="400,800,1200") | safe }}
        //
        // Generates width-based image style URLs (w400, w800, w1200) for each format.
        tera.register_filter(
            "responsive_image",
            |value: &tera::Value, args: &std::collections::HashMap<String, tera::Value>| {
                let url = value.as_str().unwrap_or_default();
                if url.is_empty() {
                    return Ok(tera::Value::String(String::new()));
                }
                let alt = args.get("alt").and_then(|v| v.as_str()).unwrap_or("");
                let widths_str = args
                    .get("widths")
                    .and_then(|v| v.as_str())
                    .unwrap_or("400,800,1200");
                let loading = args
                    .get("loading")
                    .and_then(|v| v.as_str())
                    .unwrap_or("lazy");

                let base = url.trim_start_matches('/');
                let clean_alt = ammonia::clean(alt);

                let mut avif_srcset = Vec::new();
                let mut webp_srcset = Vec::new();
                let mut jpeg_srcset = Vec::new();
                let mut last_w = "1200";

                for w in widths_str.split(',') {
                    let w = w.trim();
                    avif_srcset.push(format!("/files/styles/w{w}/{base}.avif {w}w"));
                    webp_srcset.push(format!("/files/styles/w{w}/{base}.webp {w}w"));
                    jpeg_srcset.push(format!("/files/styles/w{w}/{base} {w}w"));
                    last_w = w;
                }

                let html = format!(
                    "<picture>\n\
                     <source type=\"image/avif\" srcset=\"{}\">\n\
                     <source type=\"image/webp\" srcset=\"{}\">\n\
                     <img src=\"/files/styles/w{last_w}/{base}\" \
                     srcset=\"{}\" alt=\"{clean_alt}\" loading=\"{loading}\" decoding=\"async\">\n\
                     </picture>",
                    avif_srcset.join(", "),
                    webp_srcset.join(", "),
                    jpeg_srcset.join(", "),
                );

                Ok(tera::Value::String(html))
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

    /// The Tera instance of the theme this request renders with: the admin
    /// theme inside [`scope_request`] for an `/admin` path, the default theme
    /// otherwise, and the stock theme when the active one is not loaded.
    pub fn tera(&self) -> &Tera {
        let name = self.current_theme_name();
        self.themes
            .iter()
            .find(|(info, _)| info.machine_name == name)
            .map(|(_, tera)| tera)
            .unwrap_or(&self.tera)
    }

    /// The stock theme's Tera instance, whatever theme is active.
    pub fn stock_tera(&self) -> &Tera {
        &self.tera
    }

    /// The machine name of the theme this request renders with.
    pub fn current_theme_name(&self) -> String {
        let admin = ADMIN_REQUEST.try_with(|admin| *admin).unwrap_or(false);
        let active = self.active();
        if admin { active.admin } else { active.default }
    }

    /// Every theme: the stock theme first, then the deployment's.
    pub fn themes(&self) -> Vec<super::registry::ThemeInfo> {
        let mut all = vec![super::registry::ThemeInfo::stock()];
        all.extend(self.themes.iter().map(|(info, _)| info.clone()));
        all
    }

    /// One theme by machine name.
    pub fn theme(&self, machine_name: &str) -> Option<super::registry::ThemeInfo> {
        self.themes()
            .into_iter()
            .find(|t| t.machine_name == machine_name)
    }

    /// The default and admin themes in use.
    pub fn active(&self) -> ActiveThemes {
        match self.active.read() {
            Ok(active) => active.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Switch themes. A name that is not a loaded theme is the stock theme.
    pub fn set_active(&self, active: ActiveThemes) {
        let known = |name: &str| {
            if self
                .themes
                .iter()
                .any(|(info, _)| info.machine_name == name)
            {
                name.to_string()
            } else {
                super::registry::STOCK.to_string()
            }
        };
        let active = ActiveThemes {
            default: known(&active.default),
            admin: known(&active.admin),
        };
        match self.active.write() {
            Ok(mut guard) => *guard = active,
            Err(poisoned) => *poisoned.into_inner() = active,
        }
        self.suggestion_cache.clear();
    }

    /// Load each of `themes` (the stock theme is skipped) as `base_dirs`
    /// with the theme's `templates/` laid over them, so a template the theme
    /// does not provide is the stock one.
    ///
    /// A theme whose templates do not load is refused (B138): logged once at
    /// `error`, with each file that does not parse and Tera's message, and
    /// kept in [`Self::refused_themes`] so the appearance screen can say why it
    /// cannot be activated. It is never active; the site keeps the stock theme
    /// for it.
    pub fn with_themes(
        mut self,
        themes: Vec<super::registry::ThemeInfo>,
        base_dirs: &[PathBuf],
        locale: Option<Arc<LocaleService>>,
    ) -> Self {
        for info in themes {
            let Some(templates) = info.templates_dir() else {
                continue;
            };
            let mut dirs = base_dirs.to_vec();
            dirs.push(templates);
            match Self::build_tera(&dirs, locale.clone()) {
                Ok(tera) => self.themes.push((info, tera)),
                Err(e) => {
                    tracing::error!(
                        theme = %info.machine_name,
                        error = %e,
                        "a theme's templates do not load; the theme is refused and cannot be activated"
                    );
                    let reason = match (e.downcast_ref::<TemplateParseError>(), info.dir.as_ref()) {
                        (Some(parse), Some(dir)) => parse.relative_to(dir),
                        _ => e.to_string(),
                    };
                    self.refused.push((info, reason));
                }
            }
        }
        self
    }

    /// The deployment's themes that were refused because their templates do
    /// not load, each with the reason.
    pub fn refused_themes(&self) -> &[(super::registry::ThemeInfo, String)] {
        &self.refused
    }

    /// Why `machine_name` cannot be activated, if it is a refused theme.
    pub fn refusal(&self, machine_name: &str) -> Option<&str> {
        self.refused
            .iter()
            .find(|(info, _)| info.machine_name == machine_name)
            .map(|(_, reason)| reason.as_str())
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

        // Build cache key from suggestions, per theme: two themes may resolve
        // the same suggestions to different templates.
        let cache_key = format!("{}|{}", self.current_theme_name(), suggestions.join("|"));

        // Check cache first
        if let Some(cached) = self.metrics.cache_lookup(
            CacheName::TemplateSuggestion,
            self.suggestion_cache.get(&cache_key).map(|c| c.clone()),
        ) {
            return Some(cached);
        }

        // Find first template that exists
        for suggestion in suggestions {
            let template_name = format!("{suggestion}.html");
            if self.tera().get_template(&template_name).is_ok() {
                self.suggestion_cache
                    .insert(cache_key, template_name.clone());
                return Some(template_name);
            }

            // Also try without .html extension (in case suggestion already has it)
            if self.tera().get_template(suggestion).is_ok() {
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
        crate::telemetry::render("element", || {
            self.render_consumer.render(self.tera(), element, context)
        })
    }

    /// Render an item using template suggestions.
    pub fn render_item(&self, item: &Item, element: &RenderElement) -> Result<String> {
        crate::telemetry::render("item", || self.render_item_inner(item, element))
    }

    fn render_item_inner(&self, item: &Item, element: &RenderElement) -> Result<String> {
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

        self.tera()
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
        context.insert("libraries", &crate::form::assets::libraries_for(form));

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

        self.tera()
            .render(&template, &context)
            .context("failed to render form template")
    }

    /// Render one form element outside its form, as an AJAX response
    /// inserts it (the next value of a multi value field).
    pub fn render_form_element_html(
        &self,
        name: &str,
        element: &crate::form::FormElement,
    ) -> Result<String> {
        let empty = std::collections::HashMap::new();
        self.render_form_element(name, element, &empty, &mut tera::Context::new())
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
            let element_html = self.render_form_element(name, element, field_errors, context)?;
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
        field_errors: &std::collections::HashMap<&str, &str>,
        context: &mut tera::Context,
    ) -> Result<String> {
        use crate::form::ElementType;

        let field_error = field_errors.get(name).copied();

        let widget_template;
        let template_name = match &element.element_type {
            ElementType::Widget { widget } => {
                widget_template = format!("form/widget--{widget}.html");
                widget_template.as_str()
            }
            ElementType::Input { .. } => "form/input.html",
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
            let children_html = self.render_form_children(element, field_errors, context)?;
            el_context.insert("children", &children_html);
        }

        self.tera()
            .render(template_name, &el_context)
            .with_context(|| format!("failed to render form element: {name}"))
    }

    /// Render form element children.
    fn render_form_children(
        &self,
        element: &crate::form::FormElement,
        field_errors: &std::collections::HashMap<&str, &str>,
        context: &mut tera::Context,
    ) -> Result<String> {
        use std::fmt::Write;
        let mut html = String::new();

        let mut children: Vec<_> = element.children.iter().collect();
        children.sort_by_key(|(_, el)| el.weight);

        for (name, child) in children {
            // A field wrapped for `visible_when` is a child, and its error is
            // still its own.
            let child_html = self.render_form_element(name, child, field_errors, context)?;
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

        crate::telemetry::render("page", || {
            self.tera()
                .render(&template, context)
                .context("failed to render page template")
        })
    }

    /// Use `components` as the page builder registry, after checking that
    /// every component's template is on this engine's search path.
    ///
    /// A component whose template no directory supplies is an error naming the
    /// component, its source and the template, so the server refuses to start
    /// rather than drop the component from every page that uses it.
    pub fn with_components(
        mut self,
        components: Arc<crate::content::page_builder_components::ComponentRegistry>,
    ) -> Result<Self> {
        components.check_templates(|name| self.tera.get_template(name).is_ok())?;
        self.components = components;
        Ok(self)
    }

    /// The page builder component registry this engine renders with.
    pub fn components(&self) -> &Arc<crate::content::page_builder_components::ComponentRegistry> {
        &self.components
    }

    /// Render a Puck page builder JSON value to HTML.
    ///
    /// Parses the Puck JSON, renders each component through the template its
    /// registry definition names, sanitizes each with Ammonia, and validates
    /// heading hierarchy. `references` holds what the page's reference fields
    /// resolved to for this viewer (empty if it has none; see
    /// `routes::helpers::render_page_builder_field`).
    ///
    /// Call this from route handlers before passing content to Tera context.
    /// The rendered HTML string can then be output with `| safe` in templates,
    /// and the stylesheets linked in the page head.
    pub fn render_page_builder_content(
        &self,
        page_json: &serde_json::Value,
        references: &crate::content::page_builder::ResolvedReferences,
    ) -> Result<crate::content::page_builder::RenderedPage> {
        let page: crate::content::page_builder::PuckPage =
            serde_json::from_value(page_json.clone())
                .context("failed to parse page builder JSON")?;
        crate::content::page_builder::render_puck_page(
            &page,
            self.tera(),
            &self.components,
            references,
        )
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
        for (_, tera) in &mut self.themes {
            tera.full_reload()
                .context("failed to reload a theme's templates")?;
        }
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

    /// A select, radio set or checkbox set with no default renders, with
    /// nothing chosen: `default_value` is left out of the serialized element
    /// when it is `None`, and the templates must not compare a missing value.
    #[test]
    fn a_choice_element_without_a_default_renders() {
        use crate::form::{Form, FormElement};
        let templates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates");
        let engine = ThemeEngine::new(&[templates], None, Default::default()).unwrap();
        let options = || vec![("a".to_string(), "A".to_string())];
        let form = Form::new("probe")
            .element("pick", FormElement::select(options()))
            .element("one", FormElement::radio(options()))
            .element("many", FormElement::checkboxes(options()));
        let html = engine.render_form(&form).unwrap();
        assert!(html.contains(r#"<option value="a">A</option>"#), "{html}");
        assert!(!html.contains("selected"), "{html}");
        assert!(!html.contains("checked"), "{html}");
    }

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

    // SECURITY REGRESSION TEST — B151: the `text_format` filter sanitizes
    // full_html without needing `| safe`, and its output is never re-escaped.
    #[test]
    fn test_text_format_filter_cleans_full_html_without_safe() {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera, None);

        tera.add_raw_template("test", r#"{{ value | text_format(format="full_html") }}"#)
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert(
            "value",
            r#"<p class="lead">Hi</p><script>alert(1)</script>"#,
        );
        let result = tera.render("test", &ctx).unwrap();
        assert!(result.contains(r#"<p class="lead">Hi</p>"#), "{result}");
        assert!(
            !result.contains("&lt;p"),
            "escaped despite is_safe: {result}"
        );
        assert!(!result.contains("script"), "{result}");
    }

    #[test]
    fn test_text_format_filter_is_safe() {
        use tera::Filter;
        assert!(TextFormatFilter.is_safe());
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

    #[test]
    fn markdown_filter_renders_basic_markdown() {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera, None);

        tera.add_raw_template("test", "{{ body | markdown | safe }}")
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("body", "# Hello\n\nThis is **bold** and *italic*.");
        let result = tera.render("test", &ctx).unwrap();

        assert!(result.contains("<h1>Hello</h1>"));
        assert!(result.contains("<strong>bold</strong>"));
        assert!(result.contains("<em>italic</em>"));
    }

    #[test]
    fn markdown_filter_renders_links_and_lists() {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera, None);

        tera.add_raw_template("test", "{{ body | markdown | safe }}")
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("body", "- Item 1\n- [Link](https://example.com)");
        let result = tera.render("test", &ctx).unwrap();

        assert!(result.contains("<li>"));
        assert!(result.contains("https://example.com"));
    }

    #[test]
    fn markdown_filter_sanitizes_dangerous_html() {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera, None);

        tera.add_raw_template("test", "{{ body | markdown | safe }}")
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("body", "Hello <script>alert('xss')</script> world");
        let result = tera.render("test", &ctx).unwrap();

        assert!(
            !result.contains("<script>"),
            "script tags must be stripped: {result}"
        );
        assert!(result.contains("Hello"));
        assert!(result.contains("world"));
    }

    /// Render `body` through the real `markdown` filter.
    fn markdown(body: &str) -> String {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera, None);
        tera.add_raw_template("test", "{{ body | markdown | safe }}")
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("body", body);
        tera.render("test", &ctx).unwrap()
    }

    /// The defect, stated directly: `ammonia::clean`'s defaults stripped every
    /// class, so the language of a fenced block was erased and no highlighter
    /// could tell what it was.
    #[test]
    fn a_fenced_block_keeps_its_language_class() {
        let result = markdown("```rust\nfn main() {}\n```");
        assert!(
            result.contains("class=\"language-rust\""),
            "the language class is what makes highlighting possible: {result}"
        );
        assert!(result.contains("<code"), "got {result}");
    }

    /// The other half of the bargain: allowing `class` back must not allow it
    /// everywhere.
    #[test]
    fn an_arbitrary_class_on_a_div_is_still_stripped() {
        let result = markdown("<div class=\"totally-fine\">text</div>");
        assert!(
            !result.contains("totally-fine"),
            "class is allowed on code, pre and span only: {result}"
        );
        assert!(result.contains("text"), "the content survives: {result}");
    }

    /// A class on an allowed element is still filtered by value: `class` on
    /// `code` is not a free-form attribute.
    #[test]
    fn an_arbitrary_class_on_code_is_stripped() {
        let result = markdown("<code class=\"evil sneaky\">x</code>");
        assert!(!result.contains("evil"), "got {result}");
        assert!(!result.contains("sneaky"), "got {result}");
        assert!(
            !result.contains("class=\"\""),
            "an emptied allowlist drops the attribute rather than leaving it empty: {result}"
        );
    }

    /// A block carrying both a language and a stray class keeps only the
    /// language.
    #[test]
    fn a_mixed_class_list_keeps_only_what_is_allowed() {
        let result = markdown("<code class=\"language-rust pwned\">x</code>");
        assert!(result.contains("language-rust"), "got {result}");
        assert!(!result.contains("pwned"), "got {result}");
    }

    /// Pre-highlighted markup keeps the token classes that make it look like
    /// anything.
    #[test]
    fn highlighter_token_classes_survive_on_a_span() {
        let result = markdown(
            "<pre><code class=\"language-rust\"><span class=\"token keyword\">fn</span></code></pre>",
        );
        assert!(result.contains("language-rust"), "got {result}");
        assert!(result.contains("token"), "got {result}");
        assert!(result.contains("keyword"), "got {result}");
    }

    /// The allowlist is per element: a language class belongs on the block, a
    /// token class on a span, and neither is accepted in the other's place.
    #[test]
    fn the_class_allowlist_is_per_element() {
        assert!(is_allowed_class("code", "language-rust"));
        assert!(is_allowed_class("pre", "lang-rust"));
        assert!(is_allowed_class("span", "token"));
        assert!(is_allowed_class("span", "hljs-keyword"));

        assert!(
            !is_allowed_class("span", "language-rust"),
            "a language belongs on the block, not on a token span"
        );
        assert!(
            !is_allowed_class("code", "token"),
            "a token class belongs on a span"
        );
        assert!(!is_allowed_class("div", "language-rust"));
        assert!(!is_allowed_class("p", "token"));
    }

    /// `language-` is a prefix, not an escape hatch: the name after it is bounded
    /// to the characters real language names use.
    #[test]
    fn a_language_name_cannot_smuggle_anything() {
        assert!(is_allowed_class("code", "language-c++"));
        assert!(is_allowed_class("code", "language-c#"));
        assert!(is_allowed_class("code", "language-objective-c"));

        assert!(
            !is_allowed_class("code", "language-"),
            "empty is not a name"
        );
        assert!(!is_allowed_class("code", "language-a b"));
        assert!(!is_allowed_class("code", "language-<script>"));
        assert!(!is_allowed_class("code", "language-a/../b"));
        assert!(
            !is_allowed_class("code", &format!("language-{}", "x".repeat(33))),
            "a name long enough to be a payload is not a name"
        );
        assert!(!is_allowed_class("span", "hljs-"), "empty suffix");
        assert!(!is_allowed_class("span", "hljs-a b"));
    }

    /// Everything else ammonia's defaults remove stays removed.
    #[test]
    fn allowing_class_did_not_allow_anything_else() {
        let result = markdown(
            "<code class=\"language-rust\" style=\"color:red\" onclick=\"steal()\">x</code>",
        );
        assert!(result.contains("language-rust"), "got {result}");
        assert!(!result.contains("style"), "got {result}");
        assert!(!result.contains("onclick"), "got {result}");
        assert!(!result.contains("steal"), "got {result}");
    }

    #[test]
    fn markdown_filter_handles_empty_input() {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera, None);

        tera.add_raw_template("test", "{{ body | markdown | safe }}")
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("body", "");
        let result = tera.render("test", &ctx).unwrap();

        assert!(result.trim().is_empty());
    }

    #[test]
    fn markdown_filter_handles_non_string_input() {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera, None);

        tera.add_raw_template("test", "{{ body | markdown | safe }}")
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("body", &42);
        let result = tera.render("test", &ctx).unwrap();

        assert!(result.is_empty());
    }

    #[test]
    fn responsive_image_filter_generates_picture() {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera, None);

        tera.add_raw_template(
            "test",
            r#"{{ url | responsive_image(alt="Photo", widths="400,800") | safe }}"#,
        )
        .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("url", "/uploads/photo.jpg");
        let result = tera.render("test", &ctx).unwrap();

        assert!(result.contains("<picture>"), "missing <picture>: {result}");
        assert!(
            result.contains("image/avif"),
            "missing avif source: {result}"
        );
        assert!(
            result.contains("image/webp"),
            "missing webp source: {result}"
        );
        assert!(result.contains("w400/"), "missing w400: {result}");
        assert!(result.contains("w800/"), "missing w800: {result}");
        assert!(result.contains("alt=\"Photo\""), "missing alt: {result}");
        assert!(
            result.contains("loading=\"lazy\""),
            "missing lazy: {result}"
        );
    }

    #[test]
    fn responsive_image_filter_empty_url() {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera, None);

        tera.add_raw_template("test", r#"{{ url | responsive_image(alt="X") }}"#)
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("url", "");
        let result = tera.render("test", &ctx).unwrap();
        assert!(result.is_empty());
    }

    // --- Template search path (multi-root overlay) ---

    /// Build a throwaway template root and return its path.
    fn scratch_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "trovato_tpl_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create scratch template root");
        dir
    }

    fn write_template(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create template subdir");
        }
        std::fs::write(path, body).expect("write template");
    }

    /// A template in a later root must be able to `extend` one from an earlier
    /// root.
    ///
    /// Regression test. The first cut of the search path built one Tera per
    /// directory and then `extend`ed them together, which cannot resolve
    /// inheritance across roots: constructing the overlay's Tera failed with
    /// "is inheriting from 'gather/query.html', which doesn't exist or isn't
    /// loaded", the engine fell back to `empty()`, and every page silently
    /// rendered untemplated. Cross-root inheritance is the whole point of an
    /// app overlay, so it is the thing worth pinning down.
    #[test]
    fn overlay_template_can_extend_a_base_from_an_earlier_root() {
        let kernel = scratch_root("kernel");
        let app = scratch_root("app");

        write_template(
            &kernel,
            "gather/query.html",
            "BASE[{% block body %}{% endblock %}]",
        );
        write_template(
            &app,
            "gather/query--app.html",
            "{% extends \"gather/query.html\" %}{% block body %}APP{% endblock %}",
        );

        let engine = ThemeEngine::new(&[kernel.clone(), app.clone()], None, Default::default())
            .expect("engine must build with a cross-root inheritance chain");

        let rendered = engine
            .tera
            .render("gather/query--app.html", &tera::Context::new())
            .expect("overlay template renders");
        assert_eq!(rendered, "BASE[APP]");

        std::fs::remove_dir_all(&kernel).ok();
        std::fs::remove_dir_all(&app).ok();
    }

    /// A later root wins on a name collision, so an app can override a kernel
    /// template without editing the kernel tree.
    #[test]
    fn later_root_overrides_same_named_template() {
        let kernel = scratch_root("kernel_override");
        let app = scratch_root("app_override");

        write_template(&kernel, "elements/item.html", "KERNEL");
        write_template(&app, "elements/item.html", "APP");

        let engine = ThemeEngine::new(&[kernel.clone(), app.clone()], None, Default::default())
            .expect("engine builds");
        let rendered = engine
            .tera
            .render("elements/item.html", &tera::Context::new())
            .expect("renders");
        assert_eq!(rendered, "APP", "the later root must win");

        std::fs::remove_dir_all(&kernel).ok();
        std::fs::remove_dir_all(&app).ok();
    }

    /// A single-element search path must load templates under exactly the
    /// names the old glob-based loader produced.
    #[test]
    fn single_root_keeps_slash_separated_names() {
        let root = scratch_root("single");
        write_template(&root, "gather/includes/card.html", "CARD");

        let engine = ThemeEngine::new(std::slice::from_ref(&root), None, Default::default())
            .expect("engine builds");
        let rendered = engine
            .tera
            .render("gather/includes/card.html", &tera::Context::new())
            .expect("nested template is named with forward slashes");
        assert_eq!(rendered, "CARD");

        std::fs::remove_dir_all(&root).ok();
    }

    /// A missing overlay directory is not a startup failure.
    #[test]
    fn missing_overlay_directory_is_tolerated() {
        let kernel = scratch_root("kernel_missing");
        write_template(&kernel, "page.html", "OK");

        let absent = std::env::temp_dir().join("trovato_tpl_definitely_absent_dir");
        std::fs::remove_dir_all(&absent).ok();

        let engine = ThemeEngine::new(&[kernel.clone(), absent], None, Default::default())
            .expect("absent overlay is tolerated");
        assert_eq!(
            engine
                .tera
                .render("page.html", &tera::Context::new())
                .expect("renders"),
            "OK"
        );

        std::fs::remove_dir_all(&kernel).ok();
    }

    #[test]
    fn empty_search_path_is_rejected() {
        assert!(ThemeEngine::new(&[], None, Default::default()).is_err());
    }

    // --- B138: a template that does not parse is named, not swallowed ---

    /// A search path with one good template and one with a syntax error
    /// fails to build, and the error's own message (what a one-line log
    /// shows) names the broken file's path and carries Tera's parse message,
    /// with the line.
    #[test]
    fn a_template_that_does_not_parse_is_named_with_its_path_and_line() {
        let root = scratch_root("b138_broken");
        write_template(&root, "page.html", "GOOD {{ title }}");
        write_template(
            &root,
            "elements/broken.html",
            "line one\n{% if x %}\nno endif",
        );

        let err = ThemeEngine::new(std::slice::from_ref(&root), None, Default::default())
            .expect_err("a template that does not parse fails the build");
        let message = err.to_string();
        let path = root.join("elements/broken.html");
        assert!(
            message.contains(&path.display().to_string()),
            "the error names the file: {message}"
        );
        assert!(
            message.contains("3:"),
            "the error carries Tera's parse message with the line: {message}"
        );
        assert!(
            !message.contains("page.html"),
            "the good template is not blamed: {message}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Every broken template is named at once, so an operator fixes them in
    /// one pass rather than one restart per file.
    #[test]
    fn every_template_that_does_not_parse_is_named() {
        let kernel = scratch_root("b138_kernel");
        let app = scratch_root("b138_app");
        write_template(&kernel, "a.html", "{{ unclosed ");
        write_template(&kernel, "b.html", "fine");
        write_template(&app, "c.html", "{% block x %}no end");

        let err = ThemeEngine::new(&[kernel.clone(), app.clone()], None, Default::default())
            .expect_err("broken templates fail the build");
        let message = err.to_string();
        for broken in [kernel.join("a.html"), app.join("c.html")] {
            assert!(
                message.contains(&broken.display().to_string()),
                "{} is named: {message}",
                broken.display()
            );
        }

        std::fs::remove_dir_all(&kernel).ok();
        std::fs::remove_dir_all(&app).ok();
    }

    /// A broken template that a later root replaces is never loaded, so it
    /// does not fail the build: override rules are unchanged.
    #[test]
    fn a_broken_template_overridden_by_a_later_root_is_not_loaded() {
        let kernel = scratch_root("b138_shadowed_kernel");
        let app = scratch_root("b138_shadowed_app");
        write_template(&kernel, "page.html", "{% if broken %}");
        write_template(&app, "page.html", "APP");

        let engine = ThemeEngine::new(&[kernel.clone(), app.clone()], None, Default::default())
            .expect("the overridden template is not parsed");
        assert_eq!(
            engine
                .tera
                .render("page.html", &tera::Context::new())
                .unwrap(),
            "APP"
        );

        std::fs::remove_dir_all(&kernel).ok();
        std::fs::remove_dir_all(&app).ok();
    }

    /// A deployment theme whose template does not parse is refused: it is
    /// not offered as a theme, the reason names its file relative to the
    /// theme directory with Tera's message, choosing it leaves the stock
    /// theme active, and the stock theme still renders.
    #[test]
    fn a_theme_that_does_not_parse_is_refused_and_the_stock_theme_renders() {
        let stock = scratch_root("b138_stock");
        let themes_root = scratch_root("b138_themes");
        write_template(&stock, "base.html", "STOCK");
        let dir = themes_root.join("cracked");
        write_template(&dir, "templates/base.html", "{% if title %}CRACKED");

        let info = super::super::registry::ThemeInfo {
            machine_name: "cracked".into(),
            name: "Cracked".into(),
            description: String::new(),
            screenshot: None,
            dir: Some(dir.clone()),
        };
        let engine = ThemeEngine::new(std::slice::from_ref(&stock), None, Default::default())
            .expect("the stock theme builds")
            .with_themes(vec![info], std::slice::from_ref(&stock), None);

        assert!(engine.theme("cracked").is_none(), "not offered as a theme");
        let reason = engine.refusal("cracked").expect("refused, with a reason");
        assert!(
            reason.starts_with("template templates/base.html does not parse"),
            "names the file relative to the theme: {reason}"
        );
        assert!(reason.contains("1:"), "with Tera's line: {reason}");
        assert!(
            !reason.contains(&themes_root.display().to_string()),
            "no server path: {reason}"
        );

        engine.set_active(ActiveThemes {
            default: "cracked".into(),
            admin: "stock".into(),
        });
        assert_eq!(engine.active().default, "stock");
        assert_eq!(
            engine
                .tera()
                .render("base.html", &tera::Context::new())
                .unwrap(),
            "STOCK"
        );

        std::fs::remove_dir_all(&stock).ok();
        std::fs::remove_dir_all(&themes_root).ok();
    }

    /// B123: a stored HTML body through `plain_text` is its text, escaped
    /// once by autoescape: no tag, no script content, no `&amp;amp;`.
    #[test]
    fn plain_text_filter_gives_text_escaped_once() {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera, None);
        tera.add_raw_template("t.html", "{{ v | plain_text | truncate(length=200) }}")
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert(
            "v",
            "<p>Tom &amp; Jerry <script>alert(1)</script>said \"hi\"</p>\n<p>&lt;mark&gt;</p>",
        );
        assert_eq!(
            tera.render("t.html", &ctx).unwrap(),
            "Tom &amp; Jerry said &quot;hi&quot; &lt;mark&gt;"
        );
    }

    /// Render `{{ v | render_blocks | safe }}` with `v` bound to `value`.
    fn render_blocks_filter(value: serde_json::Value) -> String {
        let mut tera = Tera::default();
        ThemeEngine::register_filters(&mut tera, None);
        tera.add_raw_template("t", "{{ v | render_blocks | safe }}")
            .unwrap();
        let mut ctx = tera::Context::new();
        ctx.insert("v", &value);
        tera.render("t", &ctx).unwrap()
    }

    /// A string reaching `render_blocks` is not blocks, and it is not markup:
    /// it renders as text, so nothing in it can execute under `| safe`.
    #[test]
    fn render_blocks_filter_escapes_a_plain_string() {
        let html = render_blocks_filter(serde_json::json!(
            "Hi <script>alert(1)</script> and <img src=x onerror=alert(1)>"
        ));
        assert!(!html.contains("<script"), "a script tag survived: {html}");
        assert!(!html.contains("<img"), "an img tag survived: {html}");
        assert_eq!(
            html,
            "Hi &lt;script&gt;alert(1)&lt;/script&gt; and &lt;img src=x onerror=alert(1)&gt;"
        );
    }

    /// An array of blocks still renders exactly as `block_render` renders it
    /// (the `render_paragraph_with_inline_html` fixture).
    #[test]
    fn render_blocks_filter_renders_blocks_unchanged() {
        let html = render_blocks_filter(serde_json::json!([{
            "type": "paragraph",
            "data": { "text": "This is <b>bold</b> and <i>italic</i>." }
        }]));
        assert_eq!(html, "<p>This is <b>bold</b> and <i>italic</i>.</p>");
    }
}
