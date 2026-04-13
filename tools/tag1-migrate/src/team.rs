//! Team member migration.
//!
//! Reads `_data/team.json` and creates `team_member` items.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct TeamMember {
    pub firstname: String,
    pub lastname: String,
    pub shortname: String,
    pub title: String,
    #[serde(default)]
    pub bio: String,
    #[serde(default)]
    pub bio_highlight: String,
    #[serde(default)]
    pub drupal: String,
    #[serde(default)]
    pub linkedin: String,
    #[serde(default)]
    pub mastodon: String,
    #[serde(default)]
    pub bluesky: String,
    #[serde(default)]
    pub twitter: String,
}

/// Migrate team members from `_data/team.json`.
///
/// Returns a map of email/shortname → item UUID for blog author lookups.
pub async fn migrate_team(
    source: &Path,
    pool: &PgPool,
    limit: usize,
    dry_run: bool,
) -> Result<HashMap<String, Uuid>> {
    let team_path = source.join("_data/team.json");
    let content = std::fs::read_to_string(&team_path)
        .with_context(|| format!("failed to read {}", team_path.display()))?;

    let categories: HashMap<String, Vec<TeamMember>> =
        serde_json::from_str(&content).context("failed to parse team.json")?;

    let mut team_map = HashMap::new();
    let mut count = 0;
    let now = chrono::Utc::now().timestamp();

    for (_category, members) in &categories {
        for member in members {
            if limit > 0 && count >= limit {
                break;
            }

            let full_name = format!("{} {}", member.firstname, member.lastname);
            let id = Uuid::now_v7();

            // Build fields JSONB
            let fields = serde_json::json!({
                "field_first_name": {"value": member.firstname},
                "field_last_name": {"value": member.lastname},
                "field_shortname": {"value": member.shortname},
                "field_role": {"value": member.title},
                "field_bio": {"value": member.bio, "format": "filtered_html"},
                "field_bio_highlight": {"value": member.bio_highlight},
                "field_linkedin_url": {"value": member.linkedin},
                "field_drupalorg_url": {"value": member.drupal},
                "field_mastodon_url": {"value": member.mastodon},
                "field_bluesky_url": {"value": member.bluesky},
                "field_twitter_url": {"value": member.twitter},
            });

            if dry_run {
                tracing::info!(name = %full_name, shortname = %member.shortname, "would create team member");
            } else {
                sqlx::query(
                    "INSERT INTO item (id, type, title, status, author_id, fields, \
                     stage_id, created, changed) \
                     VALUES ($1, 'team_member', $2, 1, $3, $4, $5, $6, $6) \
                     ON CONFLICT DO NOTHING",
                )
                .bind(id)
                .bind(&full_name)
                .bind(Uuid::nil()) // system author
                .bind(&fields)
                .bind(live_stage_id())
                .bind(now)
                .execute(pool)
                .await?;

                // Create URL alias
                let alias = format!("/why-tag1/team/{}", member.shortname);
                create_alias(pool, &format!("/item/{id}"), &alias, now).await?;

                tracing::debug!(name = %full_name, "created team member");
            }

            // Map shortname → UUID for author lookups
            team_map.insert(member.shortname.clone(), id);
            // Also map by email-like patterns used in blog frontmatter
            let email_key = format!(
                "{}@tag1consulting.com",
                member.firstname.to_lowercase()
            );
            team_map.insert(email_key, id);

            count += 1;
        }
    }

    tracing::info!(count, "team members processed");
    Ok(team_map)
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
