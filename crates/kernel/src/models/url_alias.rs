//! URL Alias model for human-readable URLs.
//!
//! Maps alias paths (e.g., /about-us) to source paths (e.g., /item/{uuid}).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// URL Alias record.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct UrlAlias {
    /// Unique identifier (UUIDv7).
    pub id: Uuid,

    /// Source path (e.g., "/item/550e8400-e29b-41d4-a716-446655440000").
    pub source: String,

    /// Alias path (e.g., "/about-us").
    pub alias: String,

    /// Language code (default: "en").
    pub language: String,

    /// Stage ID (default: "live").
    pub stage_id: String,

    /// Unix timestamp when created.
    pub created: i64,
}

/// Input for creating a URL alias.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateUrlAlias {
    pub source: String,
    pub alias: String,
    pub language: Option<String>,
    pub stage_id: Option<String>,
}

/// Input for updating a URL alias.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateUrlAlias {
    pub source: Option<String>,
    pub alias: Option<String>,
    pub language: Option<String>,
    pub stage_id: Option<String>,
}

impl UrlAlias {
    /// Create a new URL alias.
    pub async fn create(pool: &PgPool, input: CreateUrlAlias) -> Result<Self> {
        let id = Uuid::now_v7();
        let now = chrono::Utc::now().timestamp();
        let language = input.language.unwrap_or_else(|| "en".to_string());
        let stage_id = input.stage_id.unwrap_or_else(|| "live".to_string());

        let alias = sqlx::query_as::<_, UrlAlias>(
            r#"
            INSERT INTO url_alias (id, source, alias, language, stage_id, created)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, source, alias, language, stage_id, created
            "#,
        )
        .bind(id)
        .bind(&input.source)
        .bind(&input.alias)
        .bind(&language)
        .bind(&stage_id)
        .bind(now)
        .fetch_one(pool)
        .await
        .context("failed to create url alias")?;

        Ok(alias)
    }

    /// Find a URL alias by ID.
    pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Self>> {
        let alias = sqlx::query_as::<_, UrlAlias>(
            "SELECT id, source, alias, language, stage_id, created FROM url_alias WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch url alias by id")?;

        Ok(alias)
    }

    /// Find a URL alias by alias path (for route resolution).
    /// Uses stage_id = 'live' and language = 'en' by default.
    pub async fn find_by_alias(pool: &PgPool, alias_path: &str) -> Result<Option<Self>> {
        Self::find_by_alias_with_context(pool, alias_path, "live", "en").await
    }

    /// Find a URL alias by alias path with specific stage and language.
    pub async fn find_by_alias_with_context(
        pool: &PgPool,
        alias_path: &str,
        stage_id: &str,
        language: &str,
    ) -> Result<Option<Self>> {
        let alias = sqlx::query_as::<_, UrlAlias>(
            r#"
            SELECT id, source, alias, language, stage_id, created
            FROM url_alias
            WHERE alias = $1 AND stage_id = $2 AND language = $3
            "#,
        )
        .bind(alias_path)
        .bind(stage_id)
        .bind(language)
        .fetch_optional(pool)
        .await
        .context("failed to fetch url alias by alias path")?;

        Ok(alias)
    }

    /// Find all URL aliases for a given source path.
    pub async fn find_by_source(pool: &PgPool, source: &str) -> Result<Vec<Self>> {
        Self::find_by_source_with_context(pool, source, "live", "en").await
    }

    /// Find all URL aliases for a given source path with specific stage and language.
    pub async fn find_by_source_with_context(
        pool: &PgPool,
        source: &str,
        stage_id: &str,
        language: &str,
    ) -> Result<Vec<Self>> {
        let aliases = sqlx::query_as::<_, UrlAlias>(
            r#"
            SELECT id, source, alias, language, stage_id, created
            FROM url_alias
            WHERE source = $1 AND stage_id = $2 AND language = $3
            ORDER BY created DESC, id DESC
            "#,
        )
        .bind(source)
        .bind(stage_id)
        .bind(language)
        .fetch_all(pool)
        .await
        .context("failed to fetch url aliases by source")?;

        Ok(aliases)
    }

    /// Get the canonical (most recent) alias for a source path.
    /// Returns the alias path if found, otherwise None.
    pub async fn get_canonical_alias(pool: &PgPool, source: &str) -> Result<Option<String>> {
        Self::get_canonical_alias_with_context(pool, source, "live", "en").await
    }

    /// Get the canonical (most recent) alias for a source path with specific stage and language.
    pub async fn get_canonical_alias_with_context(
        pool: &PgPool,
        source: &str,
        stage_id: &str,
        language: &str,
    ) -> Result<Option<String>> {
        let alias: Option<String> = sqlx::query_scalar(
            r#"
            SELECT alias
            FROM url_alias
            WHERE source = $1 AND stage_id = $2 AND language = $3
            ORDER BY created DESC, id DESC
            LIMIT 1
            "#,
        )
        .bind(source)
        .bind(stage_id)
        .bind(language)
        .fetch_optional(pool)
        .await
        .context("failed to get canonical alias")?;

        Ok(alias)
    }

    /// Get the canonical URL for an item.
    /// Returns the alias path if found, otherwise `/item/{id}`.
    pub async fn get_canonical_url(pool: &PgPool, item_id: Uuid) -> Result<String> {
        let source = format!("/item/{item_id}");
        if let Some(alias) = Self::get_canonical_alias(pool, &source).await? {
            Ok(alias)
        } else {
            Ok(source)
        }
    }

    /// Update a URL alias.
    pub async fn update(pool: &PgPool, id: Uuid, input: UpdateUrlAlias) -> Result<Option<Self>> {
        let existing = Self::find_by_id(pool, id).await?;
        if existing.is_none() {
            return Ok(None);
        }
        let existing = existing.unwrap();

        let source = input.source.unwrap_or(existing.source);
        let alias = input.alias.unwrap_or(existing.alias);
        let language = input.language.unwrap_or(existing.language);
        let stage_id = input.stage_id.unwrap_or(existing.stage_id);

        let updated = sqlx::query_as::<_, UrlAlias>(
            r#"
            UPDATE url_alias
            SET source = $1, alias = $2, language = $3, stage_id = $4
            WHERE id = $5
            RETURNING id, source, alias, language, stage_id, created
            "#,
        )
        .bind(&source)
        .bind(&alias)
        .bind(&language)
        .bind(&stage_id)
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to update url alias")?;

        Ok(updated)
    }

    /// Delete a URL alias.
    pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool> {
        let result = sqlx::query("DELETE FROM url_alias WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to delete url alias")?;

        Ok(result.rows_affected() > 0)
    }

    /// Delete all aliases for a source path.
    pub async fn delete_by_source(pool: &PgPool, source: &str) -> Result<u64> {
        let result = sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(source)
            .execute(pool)
            .await
            .context("failed to delete url aliases by source")?;

        Ok(result.rows_affected())
    }

    /// List all URL aliases with pagination.
    pub async fn list_all(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<Self>> {
        let aliases = sqlx::query_as::<_, UrlAlias>(
            r#"
            SELECT id, source, alias, language, stage_id, created
            FROM url_alias
            ORDER BY created DESC, id DESC
            LIMIT $1 OFFSET $2
            "#,
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .context("failed to list all url aliases")?;

        Ok(aliases)
    }

    /// Count all URL aliases.
    pub async fn count_all(pool: &PgPool) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM url_alias")
            .fetch_one(pool)
            .await
            .context("failed to count all url aliases")?;

        Ok(count)
    }

    /// Find aliases that conflict with a given alias path across stages.
    ///
    /// Returns aliases in other stages (excluding `excluding_stage`) that share
    /// the same alias path and language. Used by publish conflict detection.
    pub async fn find_conflicting_aliases(
        pool: &PgPool,
        alias_path: &str,
        language: &str,
        excluding_stage: &str,
    ) -> Result<Vec<Self>> {
        let aliases = sqlx::query_as::<_, UrlAlias>(
            r#"
            SELECT id, source, alias, language, stage_id, created
            FROM url_alias
            WHERE alias = $1 AND language = $2 AND stage_id != $3 AND stage_id != 'live'
            ORDER BY created DESC
            "#,
        )
        .bind(alias_path)
        .bind(language)
        .bind(excluding_stage)
        .fetch_all(pool)
        .await
        .context("failed to find conflicting aliases")?;

        Ok(aliases)
    }

    /// Create or update an alias for a source path.
    /// If an alias already exists for this source (same stage/language), update it.
    /// Otherwise, create a new alias.
    pub async fn upsert_for_source(
        pool: &PgPool,
        source: &str,
        alias_path: &str,
        stage_id: &str,
        language: &str,
    ) -> Result<Self> {
        // Check if there's an existing alias for this source
        let existing = Self::find_by_source_with_context(pool, source, stage_id, language).await?;

        if let Some(first) = existing.into_iter().next() {
            // Update existing alias
            let updated = Self::update(
                pool,
                first.id,
                UpdateUrlAlias {
                    source: None,
                    alias: Some(alias_path.to_string()),
                    language: None,
                    stage_id: None,
                },
            )
            .await?;
            Ok(updated.expect("update should succeed for existing record"))
        } else {
            // Create new alias
            Self::create(
                pool,
                CreateUrlAlias {
                    source: source.to_string(),
                    alias: alias_path.to_string(),
                    language: Some(language.to_string()),
                    stage_id: Some(stage_id.to_string()),
                },
            )
            .await
        }
    }
}
