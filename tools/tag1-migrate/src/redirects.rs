//! Redirect migration.
//!
//! Reads `public/_redirects` (Cloudflare format) and inserts into
//! Trovato's redirect table.

use std::path::Path;

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

/// Migrate redirects from `public/_redirects`.
pub async fn migrate_redirects(
    source: &Path,
    pool: &PgPool,
    dry_run: bool,
) -> Result<usize> {
    let redirects_path = source.join("public/_redirects");
    if !redirects_path.exists() {
        tracing::warn!("_redirects file not found, skipping");
        return Ok(0);
    }

    let content = std::fs::read_to_string(&redirects_path)?;
    let now = chrono::Utc::now().timestamp();
    let mut count = 0;

    for line in content.lines() {
        let line = line.trim();

        // Skip comments and empty lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Format: /old-path  /new-path  status_code
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }

        let from = parts[0];
        let to = parts[1];
        let status = parts
            .get(2)
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(301);

        // Skip glob/splat redirects (Cloudflare-specific, not applicable)
        if from.contains('*') || to.contains(":splat") {
            tracing::debug!(from = %from, "skipping glob redirect");
            continue;
        }

        if dry_run {
            tracing::debug!(from = %from, to = %to, status, "would create redirect");
        } else {
            sqlx::query(
                "INSERT INTO redirect (id, source_path, target_path, status_code, created, changed) \
                 VALUES ($1, $2, $3, $4, $5, $5) ON CONFLICT (source_path) DO NOTHING",
            )
            .bind(Uuid::now_v7())
            .bind(from)
            .bind(to)
            .bind(status)
            .bind(now)
            .execute(pool)
            .await?;
        }

        count += 1;
    }

    Ok(count)
}
