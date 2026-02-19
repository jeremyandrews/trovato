//! ItemType model and CRUD operations.
//!
//! Content types define the structure of items (fields, settings, etc.).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

/// Content type record.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ItemType {
    /// Machine name (e.g., "blog", "page").
    /// Note: Database column is "type", queries must use "type as type_name" alias.
    #[serde(rename = "type")]
    pub type_name: String,

    /// Human-readable label.
    pub label: String,

    /// Description for admin UI.
    pub description: Option<String>,

    /// Whether items of this type have a title field.
    pub has_title: bool,

    /// Custom label for the title field.
    pub title_label: Option<String>,

    /// Plugin that defines this content type.
    pub plugin: String,

    /// Field definitions and other type settings.
    pub settings: serde_json::Value,
}

/// Input for creating a new content type.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateItemType {
    pub type_name: String,
    pub label: String,
    pub description: Option<String>,
    pub has_title: Option<bool>,
    pub title_label: Option<String>,
    pub plugin: String,
    pub settings: Option<serde_json::Value>,
}

impl ItemType {
    /// Find a content type by machine name.
    pub async fn find_by_type(pool: &PgPool, type_name: &str) -> Result<Option<Self>> {
        let item_type = sqlx::query_as::<_, ItemType>(
            "SELECT type as type_name, label, description, has_title, title_label, plugin, settings FROM item_type WHERE type = $1"
        )
        .bind(type_name)
        .fetch_optional(pool)
        .await
        .context("failed to fetch item type")?;

        Ok(item_type)
    }

    /// List all content types.
    pub async fn list(pool: &PgPool) -> Result<Vec<Self>> {
        let types = sqlx::query_as::<_, ItemType>(
            "SELECT type as type_name, label, description, has_title, title_label, plugin, settings FROM item_type ORDER BY label"
        )
        .fetch_all(pool)
        .await
        .context("failed to list item types")?;

        Ok(types)
    }

    /// List content types by plugin.
    pub async fn list_by_plugin(pool: &PgPool, plugin: &str) -> Result<Vec<Self>> {
        let types = sqlx::query_as::<_, ItemType>(
            "SELECT type as type_name, label, description, has_title, title_label, plugin, settings FROM item_type WHERE plugin = $1 ORDER BY label"
        )
        .bind(plugin)
        .fetch_all(pool)
        .await
        .context("failed to list item types by plugin")?;

        Ok(types)
    }

    /// Create a new content type.
    pub async fn create(pool: &PgPool, input: CreateItemType) -> Result<Self> {
        let item_type = sqlx::query_as::<_, ItemType>(
            r#"
            INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING type as type_name, label, description, has_title, title_label, plugin, settings
            "#,
        )
        .bind(&input.type_name)
        .bind(&input.label)
        .bind(&input.description)
        .bind(input.has_title.unwrap_or(true))
        .bind(&input.title_label)
        .bind(&input.plugin)
        .bind(input.settings.unwrap_or(serde_json::json!({})))
        .fetch_one(pool)
        .await
        .context("failed to create item type")?;

        Ok(item_type)
    }

    /// Create or update a content type (upsert).
    pub async fn upsert(pool: &PgPool, input: CreateItemType) -> Result<Self> {
        let item_type = sqlx::query_as::<_, ItemType>(
            r#"
            INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (type) DO UPDATE SET
                label = EXCLUDED.label,
                description = EXCLUDED.description,
                has_title = EXCLUDED.has_title,
                title_label = EXCLUDED.title_label,
                settings = EXCLUDED.settings
            RETURNING type as type_name, label, description, has_title, title_label, plugin, settings
            "#,
        )
        .bind(&input.type_name)
        .bind(&input.label)
        .bind(&input.description)
        .bind(input.has_title.unwrap_or(true))
        .bind(&input.title_label)
        .bind(&input.plugin)
        .bind(input.settings.unwrap_or(serde_json::json!({})))
        .fetch_one(pool)
        .await
        .context("failed to upsert item type")?;

        Ok(item_type)
    }

    /// Delete a content type.
    pub async fn delete(pool: &PgPool, type_name: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM item_type WHERE type = $1")
            .bind(type_name)
            .execute(pool)
            .await
            .context("failed to delete item type")?;

        Ok(result.rows_affected() > 0)
    }

    /// Check if a content type exists.
    pub async fn exists(pool: &PgPool, type_name: &str) -> Result<bool> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM item_type WHERE type = $1)")
                .bind(type_name)
                .fetch_one(pool)
                .await
                .context("failed to check item type existence")?;

        Ok(exists)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn create_item_type_input() {
        let input = CreateItemType {
            type_name: "blog".to_string(),
            label: "Blog Post".to_string(),
            description: Some("A blog article".to_string()),
            has_title: Some(true),
            title_label: Some("Title".to_string()),
            plugin: "blog".to_string(),
            settings: Some(serde_json::json!({"fields": []})),
        };

        assert_eq!(input.type_name, "blog");
        assert_eq!(input.label, "Blog Post");
    }
}
