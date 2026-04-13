//! Shared database helpers for migration modules.

use anyhow::Result;
use sqlx::PgPool;
use uuid::Uuid;

/// Live stage UUID matching the kernel's `LIVE_STAGE_ID`.
pub const LIVE_STAGE_UUID: &str = "0193a5a0-0000-7000-8000-000000000001";

/// Returns the live stage UUID as a parsed `Uuid`.
pub fn live_stage_id() -> Uuid {
    Uuid::parse_str(LIVE_STAGE_UUID).expect("LIVE_STAGE_UUID is valid") // Infallible: hard-coded valid UUID
}

/// Create a URL alias.
pub async fn create_alias(pool: &PgPool, source: &str, alias: &str, now: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO url_alias (id, source, alias, language, created, stage_id) \
         VALUES ($1, $2, $3, 'en', $4, $5) \
         ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(source)
    .bind(alias)
    .bind(now)
    .bind(live_stage_id())
    .execute(pool)
    .await?;
    Ok(())
}
