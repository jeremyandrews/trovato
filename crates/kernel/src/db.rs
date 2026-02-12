//! Database connection pool management.

use anyhow::{Context, Result};
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::config::Config;

// Embed migrations at compile time
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Create a PostgreSQL connection pool.
pub async fn create_pool(config: &Config) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(config.database_max_connections)
        .connect(&config.database_url)
        .await
        .context("failed to connect to PostgreSQL")?;

    Ok(pool)
}

/// Run database migrations.
pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    MIGRATOR
        .run(pool)
        .await
        .context("failed to run database migrations")?;

    Ok(())
}

/// Check if the database connection is healthy.
pub async fn check_health(pool: &PgPool) -> bool {
    sqlx::query("SELECT 1")
        .execute(pool)
        .await
        .is_ok()
}
