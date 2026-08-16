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
    /// Get a configuration value by key (default tenant).
    pub async fn get(pool: &PgPool, key: &str) -> Result<Option<serde_json::Value>> {
        Self::get_for_tenant(pool, key, crate::models::tenant::DEFAULT_TENANT_ID).await
    }

    /// Get a configuration value by key for a specific tenant.
    ///
    /// Falls back to the default tenant if the key is not found
    /// for the requested tenant.
    pub async fn get_for_tenant(
        pool: &PgPool,
        key: &str,
        tenant_id: uuid::Uuid,
    ) -> Result<Option<serde_json::Value>> {
        // Try tenant-specific config first
        let result = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT value FROM site_config WHERE key = $1 AND tenant_id = $2",
        )
        .bind(key)
        .bind(tenant_id)
        .fetch_optional(pool)
        .await
        .context("failed to get site config")?;

        if result.is_some() {
            return Ok(result);
        }

        // Fall back to default tenant
        if tenant_id != crate::models::tenant::DEFAULT_TENANT_ID {
            let fallback = sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT value FROM site_config WHERE key = $1 AND tenant_id = $2",
            )
            .bind(key)
            .bind(crate::models::tenant::DEFAULT_TENANT_ID)
            .fetch_optional(pool)
            .await
            .context("failed to get default tenant config")?;

            return Ok(fallback);
        }

        Ok(None)
    }

    /// Set a configuration value (default tenant).
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

    /// Get a config value with secret reference resolution.
    ///
    /// String values prefixed with `env:` are resolved from environment
    /// variables at read time. The prefix `literal:` escapes the `env:` prefix
    /// for values that literally start with `env:`.
    ///
    /// Examples:
    /// - `"env:OPENAI_API_KEY"` → reads `OPENAI_API_KEY` from env
    /// - `"literal:env:NOT_A_SECRET"` → returns `"env:NOT_A_SECRET"`
    /// - `"plain value"` → returns `"plain value"` unchanged
    pub async fn get_resolved(pool: &PgPool, key: &str) -> Result<Option<serde_json::Value>> {
        let value = Self::get(pool, key).await?;
        Ok(value.map(Self::resolve_secret_refs))
    }

    /// Resolve secret references in a JSON value.
    ///
    /// Recursively processes strings with `env:` and `literal:` prefixes.
    /// Non-string values are returned unchanged.
    fn resolve_secret_refs(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::String(s) => {
                serde_json::Value::String(Self::resolve_secret_string(&s))
            }
            serde_json::Value::Object(map) => {
                let resolved: serde_json::Map<String, serde_json::Value> = map
                    .into_iter()
                    .map(|(k, v)| (k, Self::resolve_secret_refs(v)))
                    .collect();
                serde_json::Value::Object(resolved)
            }
            serde_json::Value::Array(arr) => {
                serde_json::Value::Array(arr.into_iter().map(Self::resolve_secret_refs).collect())
            }
            other => other,
        }
    }

    /// Resolve a single string value's secret reference.
    fn resolve_secret_string(s: &str) -> String {
        if let Some(var_name) = s.strip_prefix("env:") {
            // Read from environment variable
            std::env::var(var_name).unwrap_or_else(|_| {
                tracing::warn!(var = var_name, "secret config references missing env var");
                String::new()
            })
        } else if let Some(rest) = s.strip_prefix("literal:") {
            // Escape mechanism: strip the literal: prefix and return the rest
            rest.to_string()
        } else {
            s.to_string()
        }
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

    /// Get the front page path.
    pub async fn front_page(pool: &PgPool) -> Result<Option<String>> {
        let value = Self::get(pool, "site_front_page").await?;
        Ok(value
            .and_then(|v| v.as_str().map(String::from))
            .filter(|s| !s.is_empty()))
    }

    /// Set the front page path (e.g., "/item/{uuid}").
    pub async fn set_front_page(pool: &PgPool, path: &str) -> Result<()> {
        Self::set(pool, "site_front_page", serde_json::json!(path)).await
    }

    /// Get all configuration as a map.
    pub async fn all(
        pool: &PgPool,
    ) -> Result<std::collections::HashMap<String, serde_json::Value>> {
        let configs =
            sqlx::query_as::<_, SiteConfig>("SELECT key, value, updated FROM site_config")
                .fetch_all(pool)
                .await
                .context("failed to get all site configs")?;

        Ok(configs.into_iter().map(|c| (c.key, c.value)).collect())
    }
}
