//! Stored text formats (B108).
//!
//! A text format says how text a user wrote is cleaned before it is shown. The
//! three the kernel ships (`plain_text`, `filtered_html`, `full_html`) were
//! defined in code; they are rows of the `text_format` table now, seeded by a
//! migration with exactly what the code defined, and a site may change which
//! tags a format allows or add formats of its own at `/admin/config/formats`.
//!
//! Rendering is synchronous and happens in many places that hold no database
//! pool, so the formats live in a process-wide registry: loaded when the
//! server's state is built, reloaded after a save here and on every other
//! server through the `TextFormats` cache generation. Until it is loaded (a
//! unit test, a command line process) the registry holds the built-in
//! definitions, which are the same as the seeded rows.
//!
//! Who may use a format is the permission `use <format>`, as it always was;
//! `plain_text` is open to everyone.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, RwLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use super::filter::{
    ConfiguredHtmlFilter, FilterPipeline, FilteredHtmlFilter, HtmlEscapeFilter, NewlineFilter,
    UrlFilter,
};

/// The format every user may use, and the fallback for an unknown one.
pub const PLAIN_TEXT: &str = "plain_text";

/// The formats the kernel ships, which cannot be deleted.
pub const BUILT_IN: [&str; 3] = ["plain_text", "filtered_html", "full_html"];

/// Tags a format may never allow: ammonia removes their content whatever the
/// allow list says, and refuses a list that names them.
pub const FORBIDDEN_TAGS: [&str; 2] = ["script", "style"];

/// How a format cleans text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterKind {
    /// Escape every character HTML gives meaning to.
    Escape,
    /// Keep only the allowed tags and attributes.
    Html,
    /// Keep the text as written. Never rendered from stored data: see
    /// [`FilterPipeline::for_format_safe`].
    None,
}

impl FilterKind {
    /// The stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            FilterKind::Escape => "escape",
            FilterKind::Html => "html",
            FilterKind::None => "none",
        }
    }

    /// Parse a stored name.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "escape" => Some(FilterKind::Escape),
            "html" => Some(FilterKind::Html),
            "none" => Some(FilterKind::None),
            _ => None,
        }
    }

    /// What an administrator reads.
    pub fn label(self) -> &'static str {
        match self {
            FilterKind::Escape => "Escape all HTML",
            FilterKind::Html => "Limit to allowed tags",
            FilterKind::None => "No filtering",
        }
    }
}

/// One text format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextFormat {
    /// Machine name, `[a-z][a-z0-9_]*`.
    pub format: String,
    /// What editors read in the format selector.
    pub name: String,
    /// Lighter formats are listed first.
    #[serde(default)]
    pub weight: i32,
    pub filter: FilterKind,
    /// For [`FilterKind::Html`]: the tags kept.
    #[serde(default)]
    pub allowed_tags: Vec<String>,
    /// For [`FilterKind::Html`]: the attributes kept, by tag.
    #[serde(default)]
    pub allowed_attributes: BTreeMap<String, Vec<String>>,
    /// Turn line breaks into `<br>`.
    #[serde(default)]
    pub convert_newlines: bool,
    /// Link bare `http(s)://` URLs.
    #[serde(default)]
    pub convert_urls: bool,
}

impl TextFormat {
    /// Whether this is one of the formats the kernel ships.
    pub fn is_built_in(&self) -> bool {
        BUILT_IN.contains(&self.format.as_str())
    }

    /// The permission that grants it, or `None` for `plain_text`, which
    /// everyone may use.
    pub fn permission(&self) -> Option<String> {
        permission_for(&self.format)
    }

    /// The pipeline that applies it.
    pub fn pipeline(&self) -> FilterPipeline {
        let mut pipeline = FilterPipeline::new();
        pipeline = match self.filter {
            FilterKind::Escape => pipeline.add(HtmlEscapeFilter),
            FilterKind::Html => pipeline.add(ConfiguredHtmlFilter::new(
                &self.allowed_tags,
                &self.allowed_attributes,
            )),
            FilterKind::None => pipeline,
        };
        if self.convert_newlines {
            pipeline = pipeline.add(NewlineFilter);
        }
        if self.convert_urls {
            pipeline = pipeline.add(UrlFilter);
        }
        pipeline
    }
}

/// The permission that grants `format`; `None` for `plain_text`.
pub fn permission_for(format: &str) -> Option<String> {
    (format != PLAIN_TEXT).then(|| format!("use {format}"))
}

/// The formats as the kernel defined them in code before they were stored:
/// what the migration seeds, and what the registry holds until it is loaded.
pub fn built_in() -> Vec<TextFormat> {
    let tags: Vec<String> = FilteredHtmlFilter::ALLOWED_TAGS
        .iter()
        .map(|t| (*t).to_string())
        .collect();
    let attributes: BTreeMap<String, Vec<String>> = FilteredHtmlFilter::ALLOWED_TAGS
        .iter()
        .filter_map(|tag| {
            let attrs = FilteredHtmlFilter::allowed_attributes(tag);
            (!attrs.is_empty()).then(|| {
                (
                    (*tag).to_string(),
                    attrs.iter().map(|a| (*a).to_string()).collect(),
                )
            })
        })
        .collect();
    vec![
        TextFormat {
            format: "filtered_html".to_string(),
            name: "Filtered HTML".to_string(),
            weight: 0,
            filter: FilterKind::Html,
            allowed_tags: tags,
            allowed_attributes: attributes,
            convert_newlines: false,
            convert_urls: true,
        },
        TextFormat {
            format: "full_html".to_string(),
            name: "Full HTML".to_string(),
            weight: 1,
            filter: FilterKind::None,
            allowed_tags: Vec::new(),
            allowed_attributes: BTreeMap::new(),
            convert_newlines: false,
            convert_urls: false,
        },
        TextFormat {
            format: "plain_text".to_string(),
            name: "Plain Text".to_string(),
            weight: 2,
            filter: FilterKind::Escape,
            allowed_tags: Vec::new(),
            allowed_attributes: BTreeMap::new(),
            convert_newlines: true,
            convert_urls: false,
        },
    ]
}

// =============================================================================
// The registry
// =============================================================================

static REGISTRY: LazyLock<RwLock<Arc<Vec<TextFormat>>>> =
    LazyLock::new(|| RwLock::new(Arc::new(sorted(built_in()))));

fn sorted(mut formats: Vec<TextFormat>) -> Vec<TextFormat> {
    formats.sort_by(|a, b| a.weight.cmp(&b.weight).then_with(|| a.name.cmp(&b.name)));
    formats
}

/// Every format, lightest first.
pub fn all() -> Arc<Vec<TextFormat>> {
    match REGISTRY.read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// One format by machine name.
pub fn get(format: &str) -> Option<TextFormat> {
    all().iter().find(|f| f.format == format).cloned()
}

/// Replace what the registry holds.
pub fn replace(formats: Vec<TextFormat>) {
    let formats = Arc::new(sorted(formats));
    match REGISTRY.write() {
        Ok(mut guard) => *guard = formats,
        Err(poisoned) => *poisoned.into_inner() = formats,
    }
}

/// Load the stored formats into the registry. An empty table (a database the
/// migration has not reached) leaves the built-in definitions in place.
pub async fn reload(pool: &PgPool) -> Result<()> {
    let formats = list(pool).await?;
    if !formats.is_empty() {
        replace(formats);
    }
    Ok(())
}

// =============================================================================
// Storage
// =============================================================================

#[derive(sqlx::FromRow)]
struct Row {
    format: String,
    name: String,
    weight: i32,
    filter: String,
    allowed_tags: serde_json::Value,
    allowed_attributes: serde_json::Value,
    convert_newlines: bool,
    convert_urls: bool,
}

impl Row {
    fn into_format(self) -> Option<TextFormat> {
        Some(TextFormat {
            format: self.format,
            name: self.name,
            weight: self.weight,
            filter: FilterKind::parse(&self.filter)?,
            allowed_tags: serde_json::from_value(self.allowed_tags).unwrap_or_default(),
            allowed_attributes: serde_json::from_value(self.allowed_attributes).unwrap_or_default(),
            convert_newlines: self.convert_newlines,
            convert_urls: self.convert_urls,
        })
    }
}

/// Every stored format, lightest first.
pub async fn list(pool: &PgPool) -> Result<Vec<TextFormat>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT format, name, weight, filter, allowed_tags, allowed_attributes, \
         convert_newlines, convert_urls FROM text_format ORDER BY weight, name",
    )
    .fetch_all(pool)
    .await
    .context("failed to list text formats")?;
    Ok(rows.into_iter().filter_map(Row::into_format).collect())
}

/// One stored format.
pub async fn find(pool: &PgPool, format: &str) -> Result<Option<TextFormat>> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT format, name, weight, filter, allowed_tags, allowed_attributes, \
         convert_newlines, convert_urls FROM text_format WHERE format = $1",
    )
    .bind(format)
    .fetch_optional(pool)
    .await
    .context("failed to load text format")?;
    Ok(row.and_then(Row::into_format))
}

/// Insert or update `format`. The caller has validated it ([`validate`]).
pub async fn save(pool: &PgPool, format: &TextFormat) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO text_format (format, name, weight, filter, allowed_tags, \
         allowed_attributes, convert_newlines, convert_urls, created, changed) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9) \
         ON CONFLICT (format) DO UPDATE SET name = EXCLUDED.name, weight = EXCLUDED.weight, \
         filter = EXCLUDED.filter, allowed_tags = EXCLUDED.allowed_tags, \
         allowed_attributes = EXCLUDED.allowed_attributes, \
         convert_newlines = EXCLUDED.convert_newlines, convert_urls = EXCLUDED.convert_urls, \
         changed = EXCLUDED.changed",
    )
    .bind(&format.format)
    .bind(&format.name)
    .bind(format.weight)
    .bind(format.filter.as_str())
    .bind(serde_json::to_value(&format.allowed_tags)?)
    .bind(serde_json::to_value(&format.allowed_attributes)?)
    .bind(format.convert_newlines)
    .bind(format.convert_urls)
    .bind(now)
    .execute(pool)
    .await
    .context("failed to save text format")?;
    Ok(())
}

/// Delete a format that is not built in; `false` when there was none to
/// delete. Its `use <format>` grants go with it.
pub async fn delete(pool: &PgPool, format: &str) -> Result<bool> {
    if BUILT_IN.contains(&format) {
        anyhow::bail!("the {format} text format is built in and cannot be deleted");
    }
    let mut tx = pool.begin().await?;
    let done = sqlx::query("DELETE FROM text_format WHERE format = $1")
        .bind(format)
        .execute(&mut *tx)
        .await
        .context("failed to delete text format")?;
    if let Some(permission) = permission_for(format) {
        sqlx::query("DELETE FROM role_permissions WHERE permission = $1")
            .bind(permission)
            .execute(&mut *tx)
            .await
            .context("failed to revoke the deleted format's permission")?;
    }
    tx.commit().await?;
    Ok(done.rows_affected() > 0)
}

/// Make the registry here and on every other server match the table: reload
/// it now, then bump the generation the other servers watch.
pub async fn changed(pool: &PgPool) {
    if let Err(e) = reload(pool).await {
        tracing::warn!(error = %e, "failed to reload text formats after a change");
    }
    crate::cache::generation::bump(crate::cache::generation::CacheKind::TextFormats);
}

/// Every problem with `format`, as `(field, message)`; empty when it may be
/// saved. `existing` is the stored version, for a built-in format whose filter
/// may not change.
pub fn validate(format: &TextFormat, existing: Option<&TextFormat>) -> Vec<(&'static str, String)> {
    let mut problems = Vec::new();
    let machine_ok = format
        .format
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase())
        && format
            .format
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && format.format.len() <= 64;
    if !machine_ok {
        problems.push((
            "format",
            "The machine name must start with a lowercase letter and use only lowercase \
             letters, digits and underscores (at most 64)."
                .to_string(),
        ));
    }
    if format.name.trim().is_empty() {
        problems.push(("name", "Name is required.".to_string()));
    }
    if let Some(existing) = existing
        && existing.is_built_in()
        && existing.filter != format.filter
    {
        problems.push((
            "filter",
            format!(
                "The filter of the built-in {} format cannot be changed.",
                existing.format
            ),
        ));
    }
    if format.filter == FilterKind::Html {
        if format.allowed_tags.is_empty() {
            problems.push((
                "allowed_tags",
                "A format that limits HTML needs at least one allowed tag.".to_string(),
            ));
        }
        for tag in &format.allowed_tags {
            if !is_name(tag) {
                problems.push(("allowed_tags", format!("'{tag}' is not a tag name.")));
            } else if FORBIDDEN_TAGS.contains(&tag.as_str()) {
                problems.push((
                    "allowed_tags",
                    format!("<{tag}> can never be allowed: its content is always removed."),
                ));
            }
        }
        for (tag, attrs) in &format.allowed_attributes {
            if !format.allowed_tags.contains(tag) {
                problems.push((
                    "allowed_attributes",
                    format!("Attributes are listed for <{tag}>, which is not an allowed tag."),
                ));
            }
            for attr in attrs {
                if !is_name(attr) {
                    problems.push((
                        "allowed_attributes",
                        format!("'{attr}' is not an attribute name."),
                    ));
                } else if attr.starts_with("on") || attr == "style" {
                    problems.push((
                        "allowed_attributes",
                        format!("The {attr} attribute can never be allowed."),
                    ));
                } else if tag == "a" && attr == "rel" {
                    problems.push((
                        "allowed_attributes",
                        "rel on <a> is set by the filter itself (noopener noreferrer).".to_string(),
                    ));
                }
            }
        }
    }
    problems
}

fn is_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Parse the allowed tags as an administrator types them: `<p> <a> em, strong`.
pub fn parse_tags(input: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    for token in input.split(|c: char| c.is_whitespace() || c == ',') {
        let tag = token
            .trim_matches(|c| c == '<' || c == '>' || c == '/')
            .to_ascii_lowercase();
        if !tag.is_empty() && !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags
}

/// The allowed tags as the form shows them.
pub fn format_tags(tags: &[String]) -> String {
    tags.iter()
        .map(|t| format!("<{t}>"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse allowed attributes, one tag per line: `a: href title target`.
pub fn parse_attributes(input: &str) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((tag, attrs)) = line.split_once(':') else {
            return Err(format!(
                "'{line}' is not of the form tag: attribute attribute …"
            ));
        };
        let tag = tag
            .trim()
            .trim_matches(|c| c == '<' || c == '>')
            .to_ascii_lowercase();
        let entry = map.entry(tag).or_default();
        for attr in attrs.split(|c: char| c.is_whitespace() || c == ',') {
            let attr = attr.trim().to_ascii_lowercase();
            if !attr.is_empty() && !entry.contains(&attr) {
                entry.push(attr);
            }
        }
    }
    map.retain(|_, attrs| !attrs.is_empty());
    Ok(map)
}

/// The allowed attributes as the form shows them.
pub fn format_attributes(attributes: &BTreeMap<String, Vec<String>>) -> String {
    attributes
        .iter()
        .map(|(tag, attrs)| format!("{tag}: {}", attrs.join(" ")))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The inputs the built-in formats must render exactly as the code
    /// pipelines did.
    pub(crate) const CORPUS: &[&str] = &[
        "plain words",
        "line one\nline two",
        "<p>Hi</p><script>alert(1)</script>",
        r#"<a href="https://example.com" onclick="x()" rel="me">link</a>"#,
        r#"<img src="/a.png" alt="A" style="x" width="3">"#,
        "Visit https://example.com/path?q=1 today",
        r#"<table><tr><td colspan="2" class="c">x</td></tr></table>"#,
        "<b>bold</b> & 'quotes' \"double\"",
        r#"<a href="javascript:alert(1)">bad</a>"#,
        "<h2>Title</h2><iframe src=x></iframe><u>u</u>",
    ];

    #[test]
    fn the_built_in_definitions_render_as_the_code_pipelines_did() {
        let built = built_in();
        let by = |name: &str| built.iter().find(|f| f.format == name).unwrap().pipeline();
        for input in CORPUS {
            assert_eq!(
                by("plain_text").process(input),
                FilterPipeline::plain_text().process(input)
            );
            assert_eq!(
                by("filtered_html").process(input),
                FilterPipeline::filtered_html().process(input),
                "{input}"
            );
            assert_eq!(
                by("full_html").process(input),
                FilterPipeline::full_html().process(input)
            );
        }
    }

    #[test]
    fn tags_and_attributes_parse_as_typed() {
        assert_eq!(
            parse_tags("<p> <a>, em  STRONG </b>"),
            vec!["p", "a", "em", "strong", "b"]
        );
        assert_eq!(format_tags(&parse_tags("p a")), "<p> <a>");
        let attrs = parse_attributes("a: href title\n<img>: src, alt\n\n").unwrap();
        assert_eq!(attrs["a"], vec!["href", "title"]);
        assert_eq!(attrs["img"], vec!["src", "alt"]);
        assert_eq!(format_attributes(&attrs), "a: href title\nimg: src alt");
        assert!(parse_attributes("a href").is_err());
    }

    #[test]
    fn validation_refuses_dangerous_or_malformed_formats() {
        let mut format = built_in()
            .into_iter()
            .find(|f| f.format == "filtered_html")
            .unwrap();
        assert!(validate(&format, None).is_empty());
        format.allowed_tags.push("script".to_string());
        format
            .allowed_attributes
            .insert("p".to_string(), vec!["onclick".to_string()]);
        format
            .allowed_attributes
            .insert("a".to_string(), vec!["rel".to_string()]);
        format
            .allowed_attributes
            .insert("blink".to_string(), vec!["x".to_string()]);
        let problems: Vec<String> = validate(&format, None)
            .into_iter()
            .map(|(_, m)| m)
            .collect();
        assert!(
            problems.iter().any(|m| m.contains("<script>")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|m| m.contains("onclick")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|m| m.contains("rel on <a>")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|m| m.contains("<blink>")),
            "{problems:?}"
        );

        let stored = built_in()
            .into_iter()
            .find(|f| f.format == "full_html")
            .unwrap();
        let mut changed = stored.clone();
        changed.filter = FilterKind::Escape;
        assert!(
            !validate(&changed, Some(&stored)).is_empty(),
            "a built-in filter is fixed"
        );

        let mut bad_name = stored;
        bad_name.format = "Bad Name".to_string();
        assert!(
            validate(&bad_name, None)
                .iter()
                .any(|(f, _)| *f == "format")
        );
    }

    #[test]
    fn plain_text_needs_no_permission() {
        assert_eq!(permission_for("plain_text"), None);
        assert_eq!(
            permission_for("filtered_html").as_deref(),
            Some("use filtered_html")
        );
    }
}
