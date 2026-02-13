//! Comment model for threaded discussions on content items.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Comment record.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Comment {
    /// Unique identifier (UUIDv7).
    pub id: Uuid,

    /// Parent item ID.
    pub item_id: Uuid,

    /// Parent comment ID (NULL for top-level comments).
    pub parent_id: Option<Uuid>,

    /// Author user ID.
    pub author_id: Uuid,

    /// Comment body.
    pub body: String,

    /// Text format for the body.
    pub body_format: String,

    /// Publication status (0 = unpublished, 1 = published).
    pub status: i16,

    /// Unix timestamp when created.
    pub created: i64,

    /// Unix timestamp when last changed.
    pub changed: i64,

    /// Thread depth for display.
    pub depth: i16,
}

/// Input for creating a comment.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateComment {
    pub item_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub author_id: Uuid,
    pub body: String,
    pub body_format: Option<String>,
    pub status: Option<i16>,
}

/// Input for updating a comment.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateComment {
    pub body: Option<String>,
    pub body_format: Option<String>,
    pub status: Option<i16>,
}

impl Comment {
    /// Create a new comment.
    pub async fn create(pool: &PgPool, input: CreateComment) -> Result<Self> {
        let id = Uuid::now_v7();
        let now = chrono::Utc::now().timestamp();
        let body_format = input
            .body_format
            .unwrap_or_else(|| "filtered_html".to_string());
        let status = input.status.unwrap_or(1);

        let comment = sqlx::query_as::<_, Comment>(
            r#"
            INSERT INTO comment (id, item_id, parent_id, author_id, body, body_format, status, created, changed)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING id, item_id, parent_id, author_id, body, body_format, status, created, changed, depth
            "#,
        )
        .bind(id)
        .bind(input.item_id)
        .bind(input.parent_id)
        .bind(input.author_id)
        .bind(&input.body)
        .bind(&body_format)
        .bind(status)
        .bind(now)
        .bind(now)
        .fetch_one(pool)
        .await
        .context("failed to create comment")?;

        Ok(comment)
    }

    /// Find a comment by ID.
    pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Self>> {
        let comment = sqlx::query_as::<_, Comment>(
            "SELECT id, item_id, parent_id, author_id, body, body_format, status, created, changed, depth FROM comment WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch comment by id")?;

        Ok(comment)
    }

    /// List comments for an item (threaded order).
    pub async fn list_for_item(pool: &PgPool, item_id: Uuid) -> Result<Vec<Self>> {
        // Order by: top-level first by created, then children nested
        let comments = sqlx::query_as::<_, Comment>(
            r#"
            WITH RECURSIVE comment_tree AS (
                -- Base case: top-level comments
                SELECT id, item_id, parent_id, author_id, body, body_format, status, created, changed, depth,
                       ARRAY[created, EXTRACT(EPOCH FROM NOW())::BIGINT - created] AS sort_path
                FROM comment
                WHERE item_id = $1 AND parent_id IS NULL AND status = 1

                UNION ALL

                -- Recursive case: replies
                SELECT c.id, c.item_id, c.parent_id, c.author_id, c.body, c.body_format, c.status, c.created, c.changed, c.depth,
                       ct.sort_path || c.created
                FROM comment c
                JOIN comment_tree ct ON c.parent_id = ct.id
                WHERE c.status = 1
            )
            SELECT id, item_id, parent_id, author_id, body, body_format, status, created, changed, depth
            FROM comment_tree
            ORDER BY sort_path
            "#,
        )
        .bind(item_id)
        .fetch_all(pool)
        .await
        .context("failed to list comments for item")?;

        Ok(comments)
    }

    /// List comments for an item with pagination (flat, newest first).
    pub async fn list_for_item_paged(
        pool: &PgPool,
        item_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>> {
        let comments = sqlx::query_as::<_, Comment>(
            r#"
            SELECT id, item_id, parent_id, author_id, body, body_format, status, created, changed, depth
            FROM comment
            WHERE item_id = $1 AND status = 1
            ORDER BY created DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(item_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .context("failed to list comments for item")?;

        Ok(comments)
    }

    /// Count comments for an item.
    pub async fn count_for_item(pool: &PgPool, item_id: Uuid) -> Result<i64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM comment WHERE item_id = $1 AND status = 1")
                .bind(item_id)
                .fetch_one(pool)
                .await
                .context("failed to count comments for item")?;

        Ok(count)
    }

    /// List all comments (for admin moderation).
    pub async fn list_all(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<Self>> {
        let comments = sqlx::query_as::<_, Comment>(
            r#"
            SELECT id, item_id, parent_id, author_id, body, body_format, status, created, changed, depth
            FROM comment
            ORDER BY created DESC
            LIMIT $1 OFFSET $2
            "#,
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .context("failed to list all comments")?;

        Ok(comments)
    }

    /// List comments by status (for moderation).
    pub async fn list_by_status(
        pool: &PgPool,
        status: i16,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>> {
        let comments = sqlx::query_as::<_, Comment>(
            r#"
            SELECT id, item_id, parent_id, author_id, body, body_format, status, created, changed, depth
            FROM comment
            WHERE status = $1
            ORDER BY created DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(status)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .context("failed to list comments by status")?;

        Ok(comments)
    }

    /// Count all comments.
    pub async fn count_all(pool: &PgPool) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM comment")
            .fetch_one(pool)
            .await
            .context("failed to count all comments")?;

        Ok(count)
    }

    /// Update a comment.
    pub async fn update(pool: &PgPool, id: Uuid, input: UpdateComment) -> Result<Option<Self>> {
        let existing = Self::find_by_id(pool, id).await?;
        if existing.is_none() {
            return Ok(None);
        }
        let existing = existing.unwrap();

        let now = chrono::Utc::now().timestamp();
        let body = input.body.unwrap_or(existing.body);
        let body_format = input.body_format.unwrap_or(existing.body_format);
        let status = input.status.unwrap_or(existing.status);

        let comment = sqlx::query_as::<_, Comment>(
            r#"
            UPDATE comment
            SET body = $1, body_format = $2, status = $3, changed = $4
            WHERE id = $5
            RETURNING id, item_id, parent_id, author_id, body, body_format, status, created, changed, depth
            "#,
        )
        .bind(&body)
        .bind(&body_format)
        .bind(status)
        .bind(now)
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to update comment")?;

        Ok(comment)
    }

    /// Delete a comment.
    pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool> {
        let result = sqlx::query("DELETE FROM comment WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to delete comment")?;

        Ok(result.rows_affected() > 0)
    }

    /// Get replies to a comment.
    pub async fn get_replies(pool: &PgPool, comment_id: Uuid) -> Result<Vec<Self>> {
        let comments = sqlx::query_as::<_, Comment>(
            r#"
            SELECT id, item_id, parent_id, author_id, body, body_format, status, created, changed, depth
            FROM comment
            WHERE parent_id = $1 AND status = 1
            ORDER BY created ASC
            "#,
        )
        .bind(comment_id)
        .fetch_all(pool)
        .await
        .context("failed to get replies")?;

        Ok(comments)
    }
}
