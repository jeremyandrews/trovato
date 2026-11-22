//! Automatic path alias generation (Pathauto).
//!
//! Generates URL aliases from configurable patterns per content type.
//! Patterns use tokens like `[title]`, `[type]`, `[yyyy]`, `[mm]`, `[dd]`.

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::url_alias::{CreateUrlAlias, UpdateUrlAlias, UrlAlias};
use crate::models::{Item, Language, SiteConfig};

/// Convert text into a URL-safe slug.
///
/// Transforms to lowercase, replaces non-alphanumeric characters with hyphens,
/// collapses consecutive hyphens, and trims leading/trailing hyphens.
pub fn slugify(text: &str) -> String {
    let slug: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    // Collapse consecutive hyphens and trim
    let mut result = String::with_capacity(slug.len());
    let mut prev_was_hyphen = true; // Start true to skip leading hyphens
    for c in slug.chars() {
        if c == '-' {
            if !prev_was_hyphen {
                result.push('-');
            }
            prev_was_hyphen = true;
        } else {
            result.push(c);
            prev_was_hyphen = false;
        }
    }

    // Trim trailing hyphen
    while result.ends_with('-') {
        result.pop();
    }

    // Truncate to reasonable length
    if result.len() > 128 {
        // result is pure ASCII (alphanumerics + hyphens from the char map above),
        // but use is_char_boundary defensively in case the logic ever changes.
        let mut end = 128;
        while end > 0 && !result.is_char_boundary(end) {
            end -= 1;
        }
        // Find a clean break point (don't cut in middle of word)
        let truncated = &result[..end];
        if let Some(last_hyphen) = truncated.rfind('-') {
            return truncated[..last_hyphen].to_string();
        }
        return truncated.to_string();
    }

    result
}

/// Expand a pattern by replacing tokens with values from an item.
///
/// Supported tokens:
/// - `[title]` — item title, slugified
/// - `[type]` — content type machine name (must be a valid machine name: `[a-z0-9_]`)
/// - `[yyyy]` — four-digit year from created date
/// - `[mm]` — two-digit month from created date
/// - `[dd]` — two-digit day from created date
///
/// `[type]` is not slugified because machine names are validated at content type
/// registration time via `is_valid_machine_name` and are already URL-safe.
pub fn expand_pattern(
    pattern: &str,
    title: &str,
    item_type: &str,
    created: DateTime<Utc>,
) -> String {
    let slug = slugify(title);

    pattern
        .replace("[title]", &slug)
        .replace("[type]", item_type)
        .replace("[yyyy]", &format!("{:04}", created.year()))
        .replace("[mm]", &format!("{:02}", created.month()))
        .replace("[dd]", &format!("{:02}", created.day()))
}

/// Generate a unique alias for an item, handling duplicates with numeric suffixes.
///
/// If `/blog/my-post` is taken, tries `/blog/my-post-1`, `/blog/my-post-2`, etc.
/// Uses a single query to find existing aliases with matching prefix to avoid
/// sequential lookups.
///
/// Rejects aliases containing path traversal sequences (`..`), double slashes,
/// or control characters.
pub async fn generate_unique_alias(pool: &PgPool, base_alias: &str) -> Result<String> {
    // Defense-in-depth: slugify() already neutralizes traversal sequences (`.` → `-`),
    // but this guard catches direct callers that bypass expand_pattern/slugify.
    anyhow::ensure!(
        !base_alias.contains("..")
            && !base_alias.contains("//")
            && !base_alias.chars().any(|c| c.is_control()),
        "alias contains invalid path sequences"
    );

    // Ensure alias starts with /
    let base = if base_alias.starts_with('/') {
        base_alias.to_string()
    } else {
        format!("/{base_alias}")
    };

    // Escape LIKE wildcards in the base before building the pattern
    let escaped_base = base
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let like_pattern = format!("{escaped_base}%");
    // Rust '\\' → SQL literal '\' → PostgreSQL ESCAPE character is backslash.
    let existing: Vec<(String,)> =
        sqlx::query_as("SELECT alias FROM url_alias WHERE alias LIKE $1 ESCAPE '\\' LIMIT 200")
            .bind(&like_pattern)
            .fetch_all(pool)
            .await
            .context("failed to check alias uniqueness")?;

    let existing_set: std::collections::HashSet<&str> =
        existing.iter().map(|(a,)| a.as_str()).collect();

    // Try the base alias first
    if !existing_set.contains(base.as_str()) {
        return Ok(base);
    }

    // Try with numeric suffixes
    for i in 1..100 {
        let candidate = format!("{base}-{i}");
        if !existing_set.contains(candidate.as_str()) {
            return Ok(candidate);
        }
    }

    // Fallback: append UUID fragment for guaranteed uniqueness
    let fragment = &Uuid::now_v7().to_string()[..8];
    Ok(format!("{base}-{fragment}"))
}

/// Look up the pathauto pattern for a content type from site config.
///
/// Returns `None` if no pattern is configured for this type.
pub async fn get_pattern(pool: &PgPool, item_type: &str) -> Result<Option<String>> {
    let patterns = SiteConfig::get(pool, "pathauto_patterns").await?;

    Ok(patterns.and_then(|v| v.get(item_type).and_then(|p| p.as_str().map(String::from))))
}

/// The alias an item's pattern expands to, with its leading slash, or `None`
/// when the expansion is empty (a title of nothing but non-ASCII, say).
fn base_alias(pattern: &str, item: &Item) -> Option<String> {
    let created = DateTime::from_timestamp(item.created, 0).unwrap_or_else(Utc::now);
    let expanded = expand_pattern(pattern, &item.title, &item.item_type, created);
    if expanded.trim_matches('/').is_empty() {
        tracing::debug!(item_id = %item.id, "skipping pathauto: expanded pattern is empty");
        return None;
    }
    Some(if expanded.starts_with('/') {
        expanded
    } else {
        format!("/{expanded}")
    })
}

/// The language an item's alias is written in: the item's own, or the site's
/// default language for an item that names none.
async fn alias_language(pool: &PgPool, item: &Item) -> Result<String> {
    if !item.language.is_empty() {
        return Ok(item.language.clone());
    }
    Ok(Language::get_default(pool)
        .await?
        .map(|l| l.id)
        .unwrap_or_else(|| "en".to_string()))
}

/// Automatically generate and create a URL alias for an item, on the item's
/// stage and in the item's language.
///
/// Does nothing if:
/// - No pattern configured for this content type
/// - The item already has an alias, on any stage and in any language (a
///   manual alias, or one a config import brought with it)
///
/// Returns the alias path if created, or `None` if skipped. Called by
/// [`crate::content::ItemService`] after every create, so an item gets its
/// alias whichever route, plugin or import made it.
pub async fn auto_alias_item(pool: &PgPool, item: &Item) -> Result<Option<String>> {
    let Some(pattern) = get_pattern(pool, &item.item_type).await? else {
        return Ok(None);
    };

    let source = format!("/item/{}", item.id);

    // Don't overwrite manually-set aliases
    if !UrlAlias::find_all_by_source(pool, &source)
        .await?
        .is_empty()
    {
        return Ok(None);
    }

    let Some(base) = base_alias(&pattern, item) else {
        return Ok(None);
    };
    let alias = generate_unique_alias(pool, &base).await?;
    let language = alias_language(pool, item).await?;

    UrlAlias::create(
        pool,
        CreateUrlAlias {
            source,
            alias: alias.clone(),
            language: Some(language),
            stage_id: Some(item.stage_id),
        },
    )
    .await
    .context("failed to create auto-generated alias")?;

    tracing::info!(
        item_id = %item.id,
        alias = %alias,
        "auto-generated path alias"
    );

    Ok(Some(alias))
}

/// Update the URL alias for an item based on the pathauto pattern.
///
/// Does nothing if:
/// - No pattern configured for this content type
/// - An alias of the item already matches the current pattern output (the
///   base or a suffixed variant such as `/blog/post-1`), on any stage
///
/// When a pathauto pattern is configured for a content type, this function
/// owns the alias for items of that type. Aliases set via the URL alias
/// admin will be overwritten if a pattern exists. To keep a manual alias,
/// remove the pathauto pattern for the content type.
///
/// The alias rewritten is the item's own on its stage and in its language
/// when it has one, and otherwise its most recent; it is left on the item's
/// stage and in its language, which repairs an alias an older release wrote
/// in `en` on the live stage whatever the item's were.
pub async fn update_alias_item(pool: &PgPool, item: &Item) -> Result<Option<String>> {
    let Some(pattern) = get_pattern(pool, &item.item_type).await? else {
        return Ok(None);
    };
    let Some(base) = base_alias(&pattern, item) else {
        return Ok(None);
    };

    let source = format!("/item/{}", item.id);
    let existing = UrlAlias::find_all_by_source(pool, &source).await?;
    let suffixed_prefix = format!("{base}-");
    if existing
        .iter()
        .any(|a| a.alias == base || a.alias.starts_with(&suffixed_prefix))
    {
        return Ok(None);
    }

    let language = alias_language(pool, item).await?;
    let Some(row) = existing
        .iter()
        .find(|a| a.stage_id == item.stage_id && a.language == language)
        .or_else(|| existing.iter().find(|a| a.stage_id == item.stage_id))
        .or_else(|| existing.first())
    else {
        // No alias exists yet — create one.
        return auto_alias_item(pool, item).await;
    };

    // Alias exists but doesn't match current pattern — regenerate
    let alias = generate_unique_alias(pool, &base).await?;
    UrlAlias::update(
        pool,
        row.id,
        UpdateUrlAlias {
            source: None,
            alias: Some(alias.clone()),
            language: Some(language),
            stage_id: Some(item.stage_id),
        },
    )
    .await
    .context("failed to update auto-generated alias")?;

    tracing::info!(
        item_id = %item.id,
        alias = %alias,
        "auto-updated path alias"
    );

    Ok(Some(alias))
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify_basic() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("My First Blog Post"), "my-first-blog-post");
    }

    #[test]
    fn test_slugify_special_chars() {
        assert_eq!(slugify("What's New?"), "what-s-new");
        assert_eq!(slugify("Item #42: The Answer"), "item-42-the-answer");
        assert_eq!(slugify("foo & bar + baz"), "foo-bar-baz");
    }

    #[test]
    fn test_slugify_consecutive_hyphens() {
        assert_eq!(slugify("hello   world"), "hello-world");
        assert_eq!(slugify("a---b"), "a-b");
    }

    #[test]
    fn test_slugify_leading_trailing() {
        assert_eq!(slugify("  hello  "), "hello");
        assert_eq!(slugify("---hello---"), "hello");
    }

    #[test]
    fn test_slugify_empty() {
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("---"), "");
    }

    #[test]
    fn test_slugify_long_text() {
        let long_title = "a".repeat(200);
        let slug = slugify(&long_title);
        assert!(slug.len() <= 128);
    }

    #[test]
    fn test_expand_pattern_title_only() {
        let dt = DateTime::parse_from_rfc3339("2026-02-20T12:00:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(expand_pattern("[title]", "My Post", "blog", dt), "my-post");
    }

    #[test]
    fn test_expand_pattern_with_type() {
        let dt = DateTime::parse_from_rfc3339("2026-02-20T12:00:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(
            expand_pattern("[type]/[title]", "Hello World", "blog", dt),
            "blog/hello-world"
        );
    }

    #[test]
    fn test_expand_pattern_with_dates() {
        let dt = DateTime::parse_from_rfc3339("2026-03-15T12:00:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(
            expand_pattern("news/[yyyy]/[mm]/[title]", "Breaking News", "news", dt),
            "news/2026/03/breaking-news"
        );
    }

    #[test]
    fn test_slugify_path_traversal() {
        // Ensure slugify neutralizes path traversal sequences.
        // Note: different inputs may collide (e.g. "../foo" and "..foo" both → "foo").
        // This is acceptable — generate_unique_alias() resolves collisions with
        // numeric suffixes.
        let slug = slugify("../../etc/passwd");
        assert!(!slug.contains(".."), "slug must not contain path traversal");
        assert_eq!(slug, "etc-passwd");
    }

    #[test]
    fn test_slugify_control_characters() {
        let slug = slugify("hello\x00world\x1b[31m");
        assert!(
            !slug.chars().any(|c| c.is_control()),
            "slug must not contain control characters"
        );
    }

    #[test]
    fn test_expand_pattern_all_tokens() {
        let dt = DateTime::parse_from_rfc3339("2026-12-25T12:00:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(
            expand_pattern(
                "[type]/[yyyy]/[mm]/[dd]/[title]",
                "Holiday Post",
                "blog",
                dt
            ),
            "blog/2026/12/25/holiday-post"
        );
    }
}
