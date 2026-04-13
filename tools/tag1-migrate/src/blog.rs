//! Blog/article migration.
//!
//! Reads Markdown files from src/blog/, src/tag1-team-talks/, src/how-to/
//! and creates blog items with PageBuilder JSON bodies.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

use crate::categories::{extract_body, extract_frontmatter};

struct ArticleSource {
    dir: &'static str,
    collection_type: &'static str,
}

const SOURCES: &[ArticleSource] = &[
    ArticleSource {
        dir: "src/blog",
        collection_type: "Blog Post",
    },
    ArticleSource {
        dir: "src/tag1-team-talks",
        collection_type: "Tag1 Team Talk",
    },
    ArticleSource {
        dir: "src/how-to",
        collection_type: "How-To Guide",
    },
];

/// Migrate all blog/article content.
pub async fn migrate_blogs(
    source: &Path,
    pool: &PgPool,
    team_map: &HashMap<String, Uuid>,
    tag_map: &HashMap<String, Uuid>,
    limit: usize,
    dry_run: bool,
) -> Result<usize> {
    let mut total = 0;
    let now = chrono::Utc::now().timestamp();

    for src in SOURCES {
        let dir = source.join(src.dir);
        if !dir.exists() {
            tracing::warn!(dir = %dir.display(), "source directory not found, skipping");
            continue;
        }

        let mut entries: Vec<_> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            if limit > 0 && total >= limit {
                return Ok(total);
            }

            let path = entry.path();
            let content = std::fs::read_to_string(&path)?;
            let Some(fm) = extract_frontmatter(&content) else {
                tracing::warn!(file = %path.display(), "no frontmatter, skipping");
                continue;
            };

            let title = fm
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled")
                .to_string();
            let body = extract_body(&content);

            // Parse date
            let date_str = fm
                .get("date")
                .and_then(|v| v.as_str())
                .unwrap_or("2020-01-01");
            let created = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
                .map(|d| {
                    d.and_hms_opt(12, 0, 0)
                        .expect("noon is valid") // Infallible: hard-coded valid time
                        .and_utc()
                        .timestamp()
                })
                .unwrap_or(now);

            // Build PageBuilder JSON — single TextBlock with the Markdown body
            let page_builder_json = serde_json::json!({
                "root": {"props": {}},
                "content": [{"type": "TextBlock", "props": {"content": body}}]
            });

            // Resolve author
            let author_key = fm
                .get("author")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let author_id = team_map.get(author_key).copied().unwrap_or(Uuid::nil());

            // Collect tag UUIDs
            let tag_ids: Vec<Uuid> = fm
                .get("tags")
                .and_then(|t| t.as_sequence())
                .map(|tags| {
                    tags.iter()
                        .filter_map(|t| t.as_str())
                        .filter_map(|name| tag_map.get(name).copied())
                        .collect()
                })
                .unwrap_or_default();

            // Image
            let image = fm
                .get("decorativeLandscapeImage")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Series info
            let series_title = fm
                .get("articleSeries")
                .and_then(|v| v.as_mapping())
                .and_then(|m| m.get(&serde_yml::Value::String("title".into())))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let fields = serde_json::json!({
                "field_body": page_builder_json,
                "field_summary": {"value": fm.get("lead").and_then(|v| v.as_str()).unwrap_or("")},
                "field_collection_type": {"value": src.collection_type},
                "field_image": {"value": image},
                "field_image_alt": {"value": title},
                "field_series_title": {"value": series_title},
                "field_tags": tag_ids.iter().map(|id| serde_json::json!({"target_id": id})).collect::<Vec<_>>(),
                "field_author": {"target_id": author_id},
            });

            if dry_run {
                tracing::info!(
                    title = %title,
                    collection_type = %src.collection_type,
                    tags = tag_ids.len(),
                    "would create article"
                );
            } else {
                let id = Uuid::now_v7();
                sqlx::query(
                    "INSERT INTO item (id, item_type, title, status, author_id, fields, \
                     stage_id, created, changed) \
                     VALUES ($1, 'blog', $2, 1, $3, $4, $5, $6, $6) \
                     ON CONFLICT DO NOTHING",
                )
                .bind(id)
                .bind(&title)
                .bind(author_id)
                .bind(&fields)
                .bind(live_stage_id())
                .bind(created)
                .execute(pool)
                .await?;

                // Create URL alias from filename
                let slug = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("untitled");
                let alias = format!("/blog/{slug}");
                create_alias(pool, &format!("/item/{id}"), &alias, created).await?;

                tracing::debug!(title = %title, "created article");
            }

            total += 1;
        }
    }

    Ok(total)
}

fn live_stage_id() -> Uuid {
    Uuid::parse_str("00000000-0000-0000-0000-000000000001")
        .expect("live stage ID is valid") // Infallible: hard-coded valid UUID
}

async fn create_alias(pool: &PgPool, source: &str, alias: &str, now: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO url_alias (id, source, alias, created) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (alias) DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(source)
    .bind(alias)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}
