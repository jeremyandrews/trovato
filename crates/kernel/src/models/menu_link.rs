//! Menu link model for stage-aware menu items.
//!
//! Represents navigational links organized into named menus (e.g., "main", "footer").
//! Each link belongs to a menu, may have a parent for hierarchical structures,
//! and is scoped to a deployment stage.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::stage::LIVE_STAGE_ID;

/// Menu link record.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct MenuLink {
    /// Unique identifier (UUIDv7).
    pub id: Uuid,

    /// Menu machine name (e.g., "main", "footer").
    pub menu_name: String,

    /// Link destination path.
    pub path: String,

    /// Display title.
    pub title: String,

    /// Optional parent link for hierarchy.
    pub parent_id: Option<Uuid>,

    /// Sort weight (lower = higher priority).
    pub weight: i32,

    /// Whether the link is hidden from rendering.
    pub hidden: bool,

    /// Plugin that owns this link.
    pub plugin: String,

    /// Stage UUID referencing category_tag(id) in the "stages" vocabulary.
    pub stage_id: Uuid,

    /// Unix timestamp when created.
    pub created: i64,

    /// Unix timestamp when last changed.
    pub changed: i64,
}

/// Input for creating a menu link.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateMenuLink {
    pub menu_name: Option<String>,
    pub path: String,
    pub title: String,
    pub parent_id: Option<Uuid>,
    pub weight: Option<i32>,
    pub hidden: Option<bool>,
    pub plugin: Option<String>,
    pub stage_id: Option<Uuid>,
}

/// Input for updating a menu link.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateMenuLink {
    pub menu_name: Option<String>,
    pub path: Option<String>,
    pub title: Option<String>,
    pub parent_id: Option<Option<Uuid>>,
    pub weight: Option<i32>,
    pub hidden: Option<bool>,
    pub plugin: Option<String>,
    pub stage_id: Option<Uuid>,
}

impl MenuLink {
    /// Create a new menu link.
    pub async fn create(pool: &PgPool, input: CreateMenuLink) -> Result<Self> {
        let id = Uuid::now_v7();
        let now = chrono::Utc::now().timestamp();
        let menu_name = input.menu_name.unwrap_or_else(|| "main".to_string());
        let weight = input.weight.unwrap_or(0);
        let hidden = input.hidden.unwrap_or(false);
        let plugin = input.plugin.unwrap_or_else(|| "core".to_string());
        let stage_id = input.stage_id.unwrap_or(LIVE_STAGE_ID);

        let link = sqlx::query_as::<_, MenuLink>(
            r#"
            INSERT INTO menu_link (id, menu_name, path, title, parent_id, weight, hidden, plugin, stage_id, created, changed)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            RETURNING id, menu_name, path, title, parent_id, weight, hidden, plugin, stage_id, created, changed
            "#,
        )
        .bind(id)
        .bind(&menu_name)
        .bind(&input.path)
        .bind(&input.title)
        .bind(input.parent_id)
        .bind(weight)
        .bind(hidden)
        .bind(&plugin)
        .bind(stage_id)
        .bind(now)
        .bind(now)
        .fetch_one(pool)
        .await
        .context("failed to create menu link")?;

        Ok(link)
    }

    /// Find a menu link by ID.
    pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Self>> {
        let link = sqlx::query_as::<_, MenuLink>(
            "SELECT id, menu_name, path, title, parent_id, weight, hidden, plugin, stage_id, created, changed FROM menu_link WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch menu link by id")?;

        Ok(link)
    }

    /// Find all menu links for a given menu and stage, ordered by weight.
    pub async fn find_by_menu_and_stage(
        pool: &PgPool,
        menu_name: &str,
        stage_id: Uuid,
    ) -> Result<Vec<Self>> {
        let links = sqlx::query_as::<_, MenuLink>(
            r#"
            SELECT id, menu_name, path, title, parent_id, weight, hidden, plugin, stage_id, created, changed
            FROM menu_link
            WHERE menu_name = $1 AND stage_id = $2
            ORDER BY weight ASC, title ASC
            "#,
        )
        .bind(menu_name)
        .bind(stage_id)
        .fetch_all(pool)
        .await
        .context("failed to fetch menu links by menu and stage")?;

        Ok(links)
    }

    /// Find a menu link by its exact path within a specific menu and stage.
    pub async fn find_by_path(
        pool: &PgPool,
        path: &str,
        menu_name: &str,
        stage_id: Uuid,
    ) -> Result<Option<Self>> {
        let link = sqlx::query_as::<_, MenuLink>(
            r#"
            SELECT id, menu_name, path, title, parent_id, weight, hidden, plugin, stage_id, created, changed
            FROM menu_link
            WHERE path = $1 AND menu_name = $2 AND stage_id = $3
            "#,
        )
        .bind(path)
        .bind(menu_name)
        .bind(stage_id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch menu link by path")?;

        Ok(link)
    }

    /// Update a menu link.
    pub async fn update(pool: &PgPool, id: Uuid, input: UpdateMenuLink) -> Result<Option<Self>> {
        let Some(existing) = Self::find_by_id(pool, id).await? else {
            return Ok(None);
        };
        let now = chrono::Utc::now().timestamp();

        let menu_name = input.menu_name.unwrap_or(existing.menu_name);
        let path = input.path.unwrap_or(existing.path);
        let title = input.title.unwrap_or(existing.title);
        let parent_id = input.parent_id.unwrap_or(existing.parent_id);
        let weight = input.weight.unwrap_or(existing.weight);
        let hidden = input.hidden.unwrap_or(existing.hidden);
        let plugin = input.plugin.unwrap_or(existing.plugin);
        let stage_id = input.stage_id.unwrap_or(existing.stage_id);

        let updated = sqlx::query_as::<_, MenuLink>(
            r#"
            UPDATE menu_link
            SET menu_name = $1, path = $2, title = $3, parent_id = $4, weight = $5,
                hidden = $6, plugin = $7, stage_id = $8, changed = $9
            WHERE id = $10
            RETURNING id, menu_name, path, title, parent_id, weight, hidden, plugin, stage_id, created, changed
            "#,
        )
        .bind(&menu_name)
        .bind(&path)
        .bind(&title)
        .bind(parent_id)
        .bind(weight)
        .bind(hidden)
        .bind(&plugin)
        .bind(stage_id)
        .bind(now)
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to update menu link")?;

        Ok(updated)
    }

    /// Delete a menu link.
    pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool> {
        let result = sqlx::query("DELETE FROM menu_link WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to delete menu link")?;

        Ok(result.rows_affected() > 0)
    }

    /// List menu links for a given menu with pagination.
    pub async fn list_by_menu(
        pool: &PgPool,
        menu_name: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>> {
        let links = sqlx::query_as::<_, MenuLink>(
            r#"
            SELECT id, menu_name, path, title, parent_id, weight, hidden, plugin, stage_id, created, changed
            FROM menu_link
            WHERE menu_name = $1
            ORDER BY weight ASC, title ASC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(menu_name)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .context("failed to list menu links by menu")?;

        Ok(links)
    }

    /// Count menu links for a given menu.
    pub async fn count_by_menu(pool: &PgPool, menu_name: &str) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM menu_link WHERE menu_name = $1")
            .bind(menu_name)
            .fetch_one(pool)
            .await
            .context("failed to count menu links by menu")?;

        Ok(count)
    }
}
