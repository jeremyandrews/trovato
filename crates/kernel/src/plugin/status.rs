//! Plugin status tracking.
//!
//! Manages the `plugin_status` table which tracks which plugins are installed,
//! their version, and whether they are enabled or disabled.

use anyhow::Result;
use sqlx::{FromRow, PgPool, Row};

/// Status values for plugins.
pub const STATUS_DISABLED: i16 = 0;
pub const STATUS_ENABLED: i16 = 1;

/// A row from the `plugin_status` table.
#[derive(Debug, Clone, FromRow)]
pub struct PluginStatus {
    pub name: String,
    pub status: i16,
    pub version: String,
    pub installed_at: i64,
    pub updated_at: i64,
}

/// Get all plugin statuses.
pub async fn get_all_statuses(pool: &PgPool) -> Result<Vec<PluginStatus>> {
    let rows = sqlx::query_as::<_, PluginStatus>(
        "SELECT name, status, version, installed_at, updated_at FROM plugin_status ORDER BY name",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Get names of all enabled plugins.
pub async fn get_enabled_names(pool: &PgPool) -> Result<Vec<String>> {
    let rows = sqlx::query("SELECT name FROM plugin_status WHERE status = $1 ORDER BY name")
        .bind(STATUS_ENABLED)
        .fetch_all(pool)
        .await?;

    Ok(rows.iter().map(|r| r.get("name")).collect())
}

/// Check if a plugin is installed (has a row in plugin_status).
pub async fn is_installed(pool: &PgPool, name: &str) -> Result<bool> {
    let row = sqlx::query("SELECT COUNT(*) as cnt FROM plugin_status WHERE name = $1")
        .bind(name)
        .fetch_one(pool)
        .await?;

    let count: i64 = row.get("cnt");
    Ok(count > 0)
}

/// Install a plugin: insert into plugin_status with status=enabled.
pub async fn install_plugin(pool: &PgPool, name: &str, version: &str) -> Result<()> {
    let now = chrono::Utc::now().timestamp();

    sqlx::query(
        "INSERT INTO plugin_status (name, status, version, installed_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (name) DO UPDATE SET version = $3, updated_at = $5",
    )
    .bind(name)
    .bind(STATUS_ENABLED)
    .bind(version)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;

    Ok(())
}

/// Set a plugin's status (enable/disable).
pub async fn set_status(pool: &PgPool, name: &str, status: i16) -> Result<bool> {
    let now = chrono::Utc::now().timestamp();

    let result =
        sqlx::query("UPDATE plugin_status SET status = $1, updated_at = $2 WHERE name = $3")
            .bind(status)
            .bind(now)
            .bind(name)
            .execute(pool)
            .await?;

    Ok(result.rows_affected() > 0)
}

/// Auto-install any plugins found on disk but not yet in the plugin_status table.
///
/// New plugins are inserted with status=enabled to preserve the current
/// "load everything on first startup" behavior.
pub async fn auto_install_new_plugins(pool: &PgPool, discovered: &[(&str, &str)]) -> Result<u64> {
    let now = chrono::Utc::now().timestamp();
    let mut count = 0u64;

    for &(name, version) in discovered {
        let result = sqlx::query(
            "INSERT INTO plugin_status (name, status, version, installed_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (name) DO NOTHING",
        )
        .bind(name)
        .bind(STATUS_ENABLED)
        .bind(version)
        .bind(now)
        .bind(now)
        .execute(pool)
        .await?;

        count += result.rows_affected();
    }

    Ok(count)
}
