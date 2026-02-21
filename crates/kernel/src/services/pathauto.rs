//! Automatic path alias generation (Pathauto).
//!
//! Generates URL aliases from configurable patterns per content type.
//! Patterns use tokens like `[title]`, `[type]`, `[yyyy]`, `[mm]`, `[dd]`.

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::SiteConfig;
use crate::models::url_alias::{CreateUrlAlias, UrlAlias};

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
        // Find a clean break point (don't cut in middle of word)
        let truncated = &result[..128];
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
/// - `[type]` — content type machine name
/// - `[yyyy]` — four-digit year from created date
/// - `[mm]` — two-digit month from created date
/// - `[dd]` — two-digit day from created date
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
pub async fn generate_unique_alias(pool: &PgPool, base_alias: &str) -> Result<String> {
    // Ensure alias starts with /
    let base = if base_alias.starts_with('/') {
        base_alias.to_string()
    } else {
        format!("/{base_alias}")
    };

    // Try the base alias first
    if UrlAlias::find_by_alias(pool, &base).await?.is_none() {
        return Ok(base);
    }

    // Try with numeric suffixes
    for i in 1..100 {
        let candidate = format!("{base}-{i}");
        if UrlAlias::find_by_alias(pool, &candidate).await?.is_none() {
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

/// Automatically generate and create a URL alias for an item.
///
/// Does nothing if:
/// - No pattern configured for this content type
/// - An alias already exists for this item (manual override)
///
/// Returns the alias path if created, or `None` if skipped.
pub async fn auto_alias_item(
    pool: &PgPool,
    item_id: Uuid,
    title: &str,
    item_type: &str,
    created_ts: i64,
) -> Result<Option<String>> {
    let created = DateTime::from_timestamp(created_ts, 0).unwrap_or_else(Utc::now);
    let Some(pattern) = get_pattern(pool, item_type).await? else {
        return Ok(None);
    };

    let source = format!("/item/{item_id}");

    // Don't overwrite manually-set aliases
    let existing = UrlAlias::find_by_source(pool, &source).await?;
    if !existing.is_empty() {
        return Ok(None);
    }

    // Expand the pattern and generate a unique alias
    let expanded = expand_pattern(&pattern, title, item_type, created);
    let alias = generate_unique_alias(pool, &expanded).await?;

    // Create the alias
    UrlAlias::create(
        pool,
        CreateUrlAlias {
            source,
            alias: alias.clone(),
            language: None,
            stage_id: None,
        },
    )
    .await
    .context("failed to create auto-generated alias")?;

    tracing::info!(
        item_id = %item_id,
        alias = %alias,
        "auto-generated path alias"
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
