//! Category/tag migration.
//!
//! Scans blog post frontmatter for tags and creates category terms.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

/// Scan all blog posts for tags and create category terms.
///
/// Returns a map of tag name → term UUID for use in blog migration.
pub async fn migrate_categories(
    source: &Path,
    pool: &PgPool,
    dry_run: bool,
) -> Result<HashMap<String, Uuid>> {
    let mut tag_set: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Scan blog posts for tags
    let blog_dir = source.join("src/blog");
    if blog_dir.exists() {
        scan_tags_in_dir(&blog_dir, &mut tag_set)?;
    }
    let talks_dir = source.join("src/tag1-team-talks");
    if talks_dir.exists() {
        scan_tags_in_dir(&talks_dir, &mut tag_set)?;
    }
    let howto_dir = source.join("src/how-to");
    if howto_dir.exists() {
        scan_tags_in_dir(&howto_dir, &mut tag_set)?;
    }

    tracing::info!(unique_tags = tag_set.len(), "found tags in content");

    let mut tag_map = HashMap::new();
    for tag_name in &tag_set {
        if dry_run {
            tag_map.insert(tag_name.clone(), Uuid::now_v7());
            continue;
        }

        // Check if term already exists
        let existing: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM category_tag WHERE name = $1 LIMIT 1",
        )
        .bind(tag_name)
        .fetch_optional(pool)
        .await?;

        if let Some((id,)) = existing {
            tag_map.insert(tag_name.clone(), id);
        } else {
            let id = Uuid::now_v7();
            let slug = slug_from_name(tag_name);
            let now = chrono::Utc::now().timestamp();
            sqlx::query(
                "INSERT INTO category_tag (id, category_id, name, slug, weight, created, changed) \
                 VALUES ($1, $2, $3, $4, 0, $5, $5) \
                 ON CONFLICT (name) DO NOTHING",
            )
            .bind(id)
            .bind(default_category_id(pool).await?)
            .bind(tag_name)
            .bind(&slug)
            .bind(now)
            .execute(pool)
            .await?;
            tag_map.insert(tag_name.clone(), id);
            tracing::debug!(tag = %tag_name, "created tag");
        }
    }

    Ok(tag_map)
}

fn scan_tags_in_dir(
    dir: &Path,
    tag_set: &mut std::collections::HashSet<String>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "md") {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Some(fm) = extract_frontmatter(&content) {
                    if let Some(tags) = fm.get("tags").and_then(|t| t.as_sequence()) {
                        for tag in tags {
                            if let Some(s) = tag.as_str() {
                                tag_set.insert(s.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Extract YAML frontmatter from a Markdown file.
pub fn extract_frontmatter(content: &str) -> Option<serde_yml::Value> {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return None;
    }
    let rest = &content[3..];
    let end = rest.find("---")?;
    let yaml_str = &rest[..end];
    serde_yml::from_str(yaml_str).ok()
}

/// Extract body content after frontmatter.
pub fn extract_body(content: &str) -> &str {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return content;
    }
    let rest = &content[3..];
    if let Some(end) = rest.find("---") {
        rest[end + 3..].trim_start()
    } else {
        content
    }
}

fn slug_from_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Get or create the default "Tags" category.
async fn default_category_id(pool: &PgPool) -> Result<Uuid> {
    let existing: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM category WHERE name = 'tags' LIMIT 1")
            .fetch_optional(pool)
            .await?;

    if let Some((id,)) = existing {
        return Ok(id);
    }

    let id = Uuid::now_v7();
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO category (id, name, label, description, created, changed) \
         VALUES ($1, 'tags', 'Tags', 'Blog post tags', $2, $2) \
         ON CONFLICT (name) DO NOTHING",
    )
    .bind(id)
    .bind(now)
    .execute(pool)
    .await?;

    Ok(id)
}
