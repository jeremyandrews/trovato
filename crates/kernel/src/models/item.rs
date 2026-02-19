//! Item model and CRUD operations.
//!
//! Items are the core content records in Trovato (like nodes in Drupal).
//! They support JSONB field storage and revision history.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Item record (content record).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Item {
    /// Unique identifier (UUIDv7).
    pub id: Uuid,

    /// Current revision ID (null for unsaved items).
    pub current_revision_id: Option<Uuid>,

    /// Content type machine name.
    #[sqlx(rename = "type")]
    #[serde(rename = "type")]
    pub item_type: String,

    /// Item title.
    pub title: String,

    /// Author user ID.
    pub author_id: Uuid,

    /// Publication status (0 = unpublished, 1 = published).
    pub status: i16,

    /// Unix timestamp when created.
    pub created: i64,

    /// Unix timestamp when last changed.
    pub changed: i64,

    /// Promote to front page flag.
    pub promote: i16,

    /// Sticky at top of lists flag.
    pub sticky: i16,

    /// Dynamic field storage (JSONB).
    pub fields: serde_json::Value,

    /// Stage ID for content staging ('live' is default).
    pub stage_id: String,

    /// Language code (default: 'en').
    pub language: String,
}

/// Item revision record.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ItemRevision {
    /// Unique identifier (UUIDv7).
    pub id: Uuid,

    /// Parent item ID.
    pub item_id: Uuid,

    /// Author of this revision.
    pub author_id: Uuid,

    /// Title at this revision.
    pub title: String,

    /// Status at this revision.
    pub status: i16,

    /// Fields at this revision.
    pub fields: serde_json::Value,

    /// Unix timestamp when this revision was created.
    pub created: i64,

    /// Revision log message.
    pub log: Option<String>,
}

/// Input for creating a new item.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateItem {
    pub item_type: String,
    pub title: String,
    pub author_id: Uuid,
    pub status: Option<i16>,
    pub promote: Option<i16>,
    pub sticky: Option<i16>,
    pub fields: Option<serde_json::Value>,
    pub stage_id: Option<String>,
    pub language: Option<String>,
    pub log: Option<String>,
}

/// Input for updating an item.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateItem {
    pub title: Option<String>,
    pub status: Option<i16>,
    pub promote: Option<i16>,
    pub sticky: Option<i16>,
    pub fields: Option<serde_json::Value>,
    pub log: Option<String>,
}

impl Item {
    /// Check if this item is published.
    pub fn is_published(&self) -> bool {
        self.status == 1
    }

    /// Check if this item is promoted to front page.
    pub fn is_promoted(&self) -> bool {
        self.promote == 1
    }

    /// Check if this item is sticky.
    pub fn is_sticky(&self) -> bool {
        self.sticky == 1
    }

    /// Find an item by ID.
    pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Self>> {
        let item = sqlx::query_as::<_, Item>(
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language FROM item WHERE id = $1"
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch item by id")?;

        Ok(item)
    }

    /// List items by content type.
    pub async fn list_by_type(pool: &PgPool, item_type: &str) -> Result<Vec<Self>> {
        let items = sqlx::query_as::<_, Item>(
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language FROM item WHERE type = $1 ORDER BY created DESC"
        )
        .bind(item_type)
        .fetch_all(pool)
        .await
        .context("failed to list items by type")?;

        Ok(items)
    }

    /// List items by author.
    pub async fn list_by_author(pool: &PgPool, author_id: Uuid) -> Result<Vec<Self>> {
        let items = sqlx::query_as::<_, Item>(
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language FROM item WHERE author_id = $1 ORDER BY created DESC"
        )
        .bind(author_id)
        .fetch_all(pool)
        .await
        .context("failed to list items by author")?;

        Ok(items)
    }

    /// List published items.
    pub async fn list_published(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<Self>> {
        let items = sqlx::query_as::<_, Item>(
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language FROM item WHERE status = 1 AND stage_id = 'live' ORDER BY sticky DESC, created DESC LIMIT $1 OFFSET $2"
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .context("failed to list published items")?;

        Ok(items)
    }

    /// Create a new item with initial revision.
    pub async fn create(pool: &PgPool, input: CreateItem) -> Result<Self> {
        let now = chrono::Utc::now().timestamp();
        let item_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();

        // Start a transaction
        let mut tx = pool.begin().await.context("failed to start transaction")?;

        // Insert item (without current_revision_id first)
        sqlx::query(
            r#"
            INSERT INTO item (id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language)
            VALUES ($1, NULL, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            "#,
        )
        .bind(item_id)
        .bind(&input.item_type)
        .bind(&input.title)
        .bind(input.author_id)
        .bind(input.status.unwrap_or(1))
        .bind(now)
        .bind(now)
        .bind(input.promote.unwrap_or(0))
        .bind(input.sticky.unwrap_or(0))
        .bind(input.fields.clone().unwrap_or(serde_json::json!({})))
        .bind(input.stage_id.as_deref().unwrap_or("live"))
        // Routes should always provide the resolved language; "en" is a last-resort safety net.
        .bind(input.language.as_deref().unwrap_or("en"))
        .execute(&mut *tx)
        .await
        .context("failed to insert item")?;

        // Insert initial revision
        sqlx::query(
            r#"
            INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, log)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(revision_id)
        .bind(item_id)
        .bind(input.author_id)
        .bind(&input.title)
        .bind(input.status.unwrap_or(1))
        .bind(input.fields.clone().unwrap_or(serde_json::json!({})))
        .bind(now)
        .bind(input.log.as_deref().unwrap_or("Initial revision"))
        .execute(&mut *tx)
        .await
        .context("failed to insert initial revision")?;

        // Update item with current_revision_id
        sqlx::query("UPDATE item SET current_revision_id = $1 WHERE id = $2")
            .bind(revision_id)
            .bind(item_id)
            .execute(&mut *tx)
            .await
            .context("failed to update item with revision id")?;

        tx.commit().await.context("failed to commit transaction")?;

        // Fetch and return the created item
        Self::find_by_id(pool, item_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to fetch created item"))
    }

    /// Update an item and create a new revision.
    pub async fn update(
        pool: &PgPool,
        id: Uuid,
        author_id: Uuid,
        input: UpdateItem,
    ) -> Result<Option<Self>> {
        let now = chrono::Utc::now().timestamp();
        let revision_id = Uuid::now_v7();

        // Fetch current item
        let Some(current) = Self::find_by_id(pool, id).await? else {
            return Ok(None);
        };

        // Merge updates with current values
        let title = input.title.unwrap_or(current.title);
        let status = input.status.unwrap_or(current.status);
        let promote = input.promote.unwrap_or(current.promote);
        let sticky = input.sticky.unwrap_or(current.sticky);
        let fields = input.fields.unwrap_or(current.fields);
        let log = input.log;

        // Start transaction
        let mut tx = pool.begin().await.context("failed to start transaction")?;

        // Create new revision FIRST (before updating item to point to it)
        sqlx::query(
            r#"
            INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, log)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(revision_id)
        .bind(id)
        .bind(author_id)
        .bind(&title)
        .bind(status)
        .bind(&fields)
        .bind(now)
        .bind(log)
        .execute(&mut *tx)
        .await
        .context("failed to insert revision")?;

        // Update item with reference to new revision
        sqlx::query(
            r#"
            UPDATE item SET
                title = $1,
                status = $2,
                changed = $3,
                promote = $4,
                sticky = $5,
                fields = $6,
                current_revision_id = $7
            WHERE id = $8
            "#,
        )
        .bind(&title)
        .bind(status)
        .bind(now)
        .bind(promote)
        .bind(sticky)
        .bind(&fields)
        .bind(revision_id)
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("failed to update item")?;

        tx.commit().await.context("failed to commit transaction")?;

        // Return updated item
        Self::find_by_id(pool, id).await
    }

    /// Delete an item and all its revisions.
    pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool> {
        // Revisions are deleted via CASCADE
        let result = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to delete item")?;

        Ok(result.rows_affected() > 0)
    }

    /// Get all revisions for an item.
    pub async fn get_revisions(pool: &PgPool, item_id: Uuid) -> Result<Vec<ItemRevision>> {
        let revisions = sqlx::query_as::<_, ItemRevision>(
            "SELECT id, item_id, author_id, title, status, fields, created, log FROM item_revision WHERE item_id = $1 ORDER BY created DESC"
        )
        .bind(item_id)
        .fetch_all(pool)
        .await
        .context("failed to fetch revisions")?;

        Ok(revisions)
    }

    /// Get a specific revision.
    pub async fn get_revision(pool: &PgPool, revision_id: Uuid) -> Result<Option<ItemRevision>> {
        let revision = sqlx::query_as::<_, ItemRevision>(
            "SELECT id, item_id, author_id, title, status, fields, created, log FROM item_revision WHERE id = $1"
        )
        .bind(revision_id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch revision")?;

        Ok(revision)
    }

    /// Revert an item to a previous revision.
    /// Creates a new revision with the old revision's content.
    pub async fn revert_to_revision(
        pool: &PgPool,
        item_id: Uuid,
        revision_id: Uuid,
        author_id: Uuid,
    ) -> Result<Self> {
        // Fetch the target revision
        let revision = Self::get_revision(pool, revision_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("revision not found"))?;

        if revision.item_id != item_id {
            anyhow::bail!("revision does not belong to this item");
        }

        // Create update input from revision
        let input = UpdateItem {
            title: Some(revision.title),
            status: Some(revision.status),
            promote: None,
            sticky: None,
            fields: Some(revision.fields),
            log: Some(format!("Reverted to revision {revision_id}")),
        };

        // Update creates a new revision with the old content
        Self::update(pool, item_id, author_id, input)
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to revert item"))
    }

    /// Count items by type.
    pub async fn count_by_type(pool: &PgPool, item_type: &str) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE type = $1")
            .bind(item_type)
            .fetch_one(pool)
            .await
            .context("failed to count items")?;

        Ok(count)
    }

    /// List all items with pagination.
    pub async fn list_all(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<Self>> {
        let items = sqlx::query_as::<_, Item>(
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language FROM item ORDER BY changed DESC LIMIT $1 OFFSET $2"
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .context("failed to list all items")?;

        Ok(items)
    }

    /// List items with optional filters.
    pub async fn list_filtered(
        pool: &PgPool,
        item_type: Option<&str>,
        status: Option<i16>,
        author_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>> {
        // Build dynamic query
        // Note: We use 'type' not 'type' because sqlx uses the #[sqlx(rename = "type")] attribute
        let mut query = String::from(
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language FROM item WHERE 1=1",
        );
        let mut param_idx = 1;
        let mut conditions = Vec::new();

        if item_type.is_some() {
            conditions.push(format!(" AND type = ${param_idx}"));
            param_idx += 1;
        }
        if status.is_some() {
            conditions.push(format!(" AND status = ${param_idx}"));
            param_idx += 1;
        }
        if author_id.is_some() {
            conditions.push(format!(" AND author_id = ${param_idx}"));
            param_idx += 1;
        }

        for cond in conditions {
            query.push_str(&cond);
        }

        query.push_str(&format!(
            " ORDER BY changed DESC LIMIT ${} OFFSET ${}",
            param_idx,
            param_idx + 1
        ));

        let mut query_builder = sqlx::query_as::<_, Item>(&query);

        if let Some(t) = item_type {
            query_builder = query_builder.bind(t);
        }
        if let Some(s) = status {
            query_builder = query_builder.bind(s);
        }
        if let Some(a) = author_id {
            query_builder = query_builder.bind(a);
        }

        query_builder = query_builder.bind(limit).bind(offset);

        let items = query_builder
            .fetch_all(pool)
            .await
            .context("failed to list filtered items")?;

        Ok(items)
    }

    /// Count items with optional filters.
    pub async fn count_filtered(
        pool: &PgPool,
        item_type: Option<&str>,
        status: Option<i16>,
        author_id: Option<Uuid>,
    ) -> Result<i64> {
        let mut query = String::from("SELECT COUNT(*) FROM item WHERE 1=1");
        let mut param_idx = 1;
        let mut conditions = Vec::new();

        if item_type.is_some() {
            conditions.push(format!(" AND type = ${param_idx}"));
            param_idx += 1;
        }
        if status.is_some() {
            conditions.push(format!(" AND status = ${param_idx}"));
            param_idx += 1;
        }
        if author_id.is_some() {
            conditions.push(format!(" AND author_id = ${param_idx}"));
        }

        for cond in conditions {
            query.push_str(&cond);
        }

        let mut query_builder = sqlx::query_scalar::<_, i64>(&query);

        if let Some(t) = item_type {
            query_builder = query_builder.bind(t);
        }
        if let Some(s) = status {
            query_builder = query_builder.bind(s);
        }
        if let Some(a) = author_id {
            query_builder = query_builder.bind(a);
        }

        let count = query_builder
            .fetch_one(pool)
            .await
            .context("failed to count filtered items")?;

        Ok(count)
    }

    /// Count all items.
    pub async fn count_all(pool: &PgPool) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item")
            .fetch_one(pool)
            .await
            .context("failed to count all items")?;

        Ok(count)
    }

    /// Count published items.
    pub async fn count_published(pool: &PgPool) -> Result<i64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE status = 1 AND stage_id = 'live'")
                .fetch_one(pool)
                .await
                .context("failed to count published items")?;

        Ok(count)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn item_status_checks() {
        let item = Item {
            id: Uuid::now_v7(),
            current_revision_id: Some(Uuid::now_v7()),
            item_type: "page".to_string(),
            title: "Test".to_string(),
            author_id: Uuid::nil(),
            status: 1,
            created: 0,
            changed: 0,
            promote: 1,
            sticky: 0,
            fields: serde_json::json!({}),
            stage_id: "live".to_string(),
            language: "en".to_string(),
        };

        assert!(item.is_published());
        assert!(item.is_promoted());
        assert!(!item.is_sticky());
    }

    #[test]
    fn create_item_input() {
        let input = CreateItem {
            item_type: "blog".to_string(),
            title: "Test Post".to_string(),
            author_id: Uuid::nil(),
            status: Some(1),
            promote: None,
            sticky: None,
            fields: Some(serde_json::json!({"body": {"value": "Hello"}})),
            stage_id: None,
            language: None,
            log: None,
        };

        assert_eq!(input.item_type, "blog");
        assert_eq!(input.title, "Test Post");
    }
}
