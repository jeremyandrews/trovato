//! Site configuration model for installation status and site settings.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

/// Site configuration record.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SiteConfig {
    /// Configuration key.
    pub key: String,

    /// Configuration value (JSON).
    pub value: serde_json::Value,

    /// When this config was last updated.
    pub updated: chrono::DateTime<chrono::Utc>,
}

impl SiteConfig {
    /// Get a configuration value by key.
    pub async fn get(pool: &PgPool, key: &str) -> Result<Option<serde_json::Value>> {
        let result = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT value FROM site_config WHERE key = $1",
        )
        .bind(key)
        .fetch_optional(pool)
        .await
        .context("failed to get site config")?;

        Ok(result)
    }

    /// Set a configuration value.
    pub async fn set(pool: &PgPool, key: &str, value: serde_json::Value) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO site_config (key, value, updated)
            VALUES ($1, $2, NOW())
            ON CONFLICT (key) DO UPDATE SET value = $2, updated = NOW()
            "#,
        )
        .bind(key)
        .bind(value)
        .execute(pool)
        .await
        .context("failed to set site config")?;

        Ok(())
    }

    /// Check if the site is installed.
    pub async fn is_installed(pool: &PgPool) -> Result<bool> {
        let value = Self::get(pool, "installed").await?;
        Ok(value.map(|v| v.as_bool().unwrap_or(false)).unwrap_or(false))
    }

    /// Mark the site as installed.
    pub async fn mark_installed(pool: &PgPool) -> Result<()> {
        Self::set(pool, "installed", serde_json::json!(true)).await
    }

    /// Get the site name.
    pub async fn site_name(pool: &PgPool) -> Result<String> {
        let value = Self::get(pool, "site_name").await?;
        Ok(value
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "Trovato".to_string()))
    }

    /// Set the site name.
    pub async fn set_site_name(pool: &PgPool, name: &str) -> Result<()> {
        Self::set(pool, "site_name", serde_json::json!(name)).await
    }

    /// Get the site slogan.
    pub async fn site_slogan(pool: &PgPool) -> Result<String> {
        let value = Self::get(pool, "site_slogan").await?;
        Ok(value
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default())
    }

    /// Set the site slogan.
    pub async fn set_site_slogan(pool: &PgPool, slogan: &str) -> Result<()> {
        Self::set(pool, "site_slogan", serde_json::json!(slogan)).await
    }

    /// Get the site email.
    pub async fn site_mail(pool: &PgPool) -> Result<String> {
        let value = Self::get(pool, "site_mail").await?;
        Ok(value
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default())
    }

    /// Set the site email.
    pub async fn set_site_mail(pool: &PgPool, mail: &str) -> Result<()> {
        Self::set(pool, "site_mail", serde_json::json!(mail)).await
    }

    /// Get all configuration as a map.
    pub async fn all(pool: &PgPool) -> Result<std::collections::HashMap<String, serde_json::Value>> {
        let configs = sqlx::query_as::<_, SiteConfig>("SELECT key, value, updated FROM site_config")
            .fetch_all(pool)
            .await
            .context("failed to get all site configs")?;

        Ok(configs.into_iter().map(|c| (c.key, c.value)).collect())
    }
}
