//! Composed page migration.
//!
//! Reads .njk files from what-we-do/, innovations/, our-work/, white-papers/
//! and creates composed_page or case_study items with PageBuilder JSON bodies.
//!
//! Nunjucks shortcodes are mapped to Puck components where possible.
//! Complex shortcode patterns that can't be automatically converted are stored
//! as TextBlock components with the raw Nunjucks source (for manual cleanup).

use std::path::Path;

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

use crate::categories::extract_frontmatter;

struct PageSource {
    dir: &'static str,
    item_type: &'static str,
    alias_prefix: &'static str,
}

const SOURCES: &[PageSource] = &[
    PageSource {
        dir: "src/what-we-do",
        item_type: "composed_page",
        alias_prefix: "/what-we-do",
    },
    PageSource {
        dir: "src/innovations",
        item_type: "composed_page",
        alias_prefix: "/innovations",
    },
    PageSource {
        dir: "src/white-papers",
        item_type: "composed_page",
        alias_prefix: "/white-papers",
    },
    PageSource {
        dir: "src/our-work/case-studies",
        item_type: "case_study",
        alias_prefix: "/our-work",
    },
];

/// Migrate composed pages and case studies from .njk files.
pub async fn migrate_pages(
    source: &Path,
    pool: &PgPool,
    limit: usize,
    dry_run: bool,
) -> Result<usize> {
    let mut total = 0;
    let now = chrono::Utc::now().timestamp();

    for src in SOURCES {
        let dir = source.join(src.dir);
        if !dir.exists() {
            tracing::warn!(dir = %dir.display(), "directory not found, skipping");
            continue;
        }

        let mut entries: Vec<_> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                let p = e.path();
                p.extension().is_some_and(|ext| ext == "njk" || ext == "md")
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n != "index.njk")
            })
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            if limit > 0 && total >= limit {
                return Ok(total);
            }

            let path = entry.path();
            let content = std::fs::read_to_string(&path)?;

            // Extract frontmatter (works for both .njk and .md)
            let fm = extract_frontmatter(&content);
            let title = fm
                .as_ref()
                .and_then(|f| f.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled")
                .to_string();

            // Check for explicit permalink in frontmatter
            let permalink = fm
                .as_ref()
                .and_then(|f| f.get("permalink"))
                .and_then(|v| v.as_str())
                .map(|s| s.trim_end_matches('/').to_string());

            // Extract the body (after frontmatter)
            let body = crate::categories::extract_body(&content);

            // Convert to PageBuilder JSON
            let page_builder_json = njk_to_puck_json(body, &title);

            let fields = if src.item_type == "case_study" {
                // Extract structured case study fields from frontmatter
                let hero = fm
                    .as_ref()
                    .and_then(|f| f.get("pageHero"))
                    .and_then(|v| v.as_mapping());
                let client = hero
                    .and_then(|h| h.get(&serde_yml::Value::String("eyebrow".into())))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                serde_json::json!({
                    "field_body": page_builder_json,
                    "field_client_name": {"value": client},
                })
            } else {
                let summary = fm
                    .as_ref()
                    .and_then(|f| f.get("pageHero"))
                    .and_then(|v| v.as_mapping())
                    .and_then(|h| h.get(&serde_yml::Value::String("text".into())))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                serde_json::json!({
                    "field_body": page_builder_json,
                    "field_summary": {"value": summary},
                })
            };

            if dry_run {
                tracing::info!(
                    title = %title,
                    item_type = %src.item_type,
                    file = %path.display(),
                    "would create page"
                );
            } else {
                let id = Uuid::now_v7();
                sqlx::query(
                    "INSERT INTO item (id, type, title, status, author_id, fields, \
                     stage_id, created, changed) \
                     VALUES ($1, $2, $3, 1, $4, $5, $6, $7, $7) \
                     ON CONFLICT DO NOTHING",
                )
                .bind(id)
                .bind(src.item_type)
                .bind(&title)
                .bind(Uuid::nil())
                .bind(&fields)
                .bind(live_stage_id())
                .bind(now)
                .execute(pool)
                .await?;

                // URL alias
                let slug = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("page");
                let alias = permalink.unwrap_or_else(|| format!("{}/{slug}", src.alias_prefix));
                create_alias(pool, &format!("/item/{id}"), &alias, now).await?;

                tracing::debug!(title = %title, alias = %alias, "created page");
            }

            total += 1;
        }
    }

    Ok(total)
}

/// Convert Nunjucks template body to Puck JSON.
///
/// For pages with simple content, wraps in a single TextBlock.
/// For pages with recognizable shortcode patterns, maps to Puck components.
fn njk_to_puck_json(body: &str, title: &str) -> serde_json::Value {
    let mut components = Vec::new();

    // Try to extract hero from the page structure
    // Many Tag1 pages start with a hero section defined in frontmatter
    // (handled separately). The body contains the rest.

    // Split body into sections based on Nunjucks shortcode boundaries.
    // For v1, we do a best-effort conversion:
    // - Recognizable shortcodes → Puck components
    // - Everything else → TextBlock with raw content

    let sections = split_njk_sections(body);

    for section in sections {
        match section {
            NjkSection::Text(text) => {
                if !text.trim().is_empty() {
                    components.push(serde_json::json!({
                        "type": "TextBlock",
                        "props": {"content": text.trim()}
                    }));
                }
            }
            NjkSection::SectionWrapper { bg_color, content } => {
                // Wrap content in a SectionWrapper with a TextBlock child
                components.push(serde_json::json!({
                    "type": "SectionWrapper",
                    "props": {
                        "backgroundColor": bg_color,
                        "padding": "default",
                        "maxWidth": "default"
                    },
                    "zones": {
                        "content": [{
                            "type": "TextBlock",
                            "props": {"content": content.trim()}
                        }]
                    }
                }));
            }
            NjkSection::Raw(raw) => {
                // Unrecognized shortcode — store as TextBlock for manual cleanup
                components.push(serde_json::json!({
                    "type": "TextBlock",
                    "props": {"content": format!("<!-- TODO: convert from Nunjucks -->\n{}", raw.trim())}
                }));
            }
        }
    }

    // If no components were extracted, create a single TextBlock with the whole body
    if components.is_empty() && !body.trim().is_empty() {
        components.push(serde_json::json!({
            "type": "TextBlock",
            "props": {"content": body.trim()}
        }));
    }

    // Add a hero at the beginning if the title is available
    if !title.is_empty() {
        components.insert(
            0,
            serde_json::json!({
                "type": "Hero",
                "props": {"title": title, "variant": "minimal", "headingLevel": 2}
            }),
        );
    }

    serde_json::json!({
        "root": {"props": {}},
        "content": components
    })
}

enum NjkSection {
    Text(String),
    SectionWrapper { bg_color: String, content: String },
    Raw(String),
}

/// Split Nunjucks body into recognizable sections.
///
/// This is a best-effort heuristic parser, not a full Nunjucks parser.
fn split_njk_sections(body: &str) -> Vec<NjkSection> {
    let mut sections = Vec::new();
    let mut current_text = String::new();

    for line in body.lines() {
        let trimmed = line.trim();

        // Detect sectionWrapper shortcodes
        if trimmed.starts_with("{%") && trimmed.contains("sectionWrapper") {
            if !current_text.trim().is_empty() {
                sections.push(NjkSection::Text(std::mem::take(&mut current_text)));
            }
            // For now, mark as raw for manual conversion
            current_text.push_str(line);
            current_text.push('\n');
        } else if trimmed.starts_with("{%") && trimmed.contains("endsectionWrapper") {
            current_text.push_str(line);
            current_text.push('\n');
            sections.push(NjkSection::Raw(std::mem::take(&mut current_text)));
        } else if trimmed.starts_with("{%") {
            // Other shortcode — include in current text
            current_text.push_str(line);
            current_text.push('\n');
        } else {
            current_text.push_str(line);
            current_text.push('\n');
        }
    }

    if !current_text.trim().is_empty() {
        // Check if it contains Nunjucks shortcodes
        if current_text.contains("{%") {
            sections.push(NjkSection::Raw(current_text));
        } else {
            sections.push(NjkSection::Text(current_text));
        }
    }

    sections
}

/// Live stage UUID matching the kernel's `LIVE_STAGE_ID`.
const LIVE_STAGE_UUID: &str = "0193a5a0-0000-7000-8000-000000000001";

fn live_stage_id() -> Uuid {
    Uuid::parse_str(LIVE_STAGE_UUID).expect("LIVE_STAGE_UUID is valid") // Infallible: hard-coded valid UUID
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
