//! Comment service with tap integration.
//!
//! Provides CRUD operations for comments with automatic tap invocations
//! for plugin taps (insert, update, delete, access).

use std::sync::Arc;

use anyhow::{Context, Result};
use sqlx::PgPool;
use tracing::info;
use uuid::Uuid;

use crate::models::{Comment, CreateComment, UpdateComment};
use crate::tap::{RequestState, TapDispatcher, UserContext};
use trovato_sdk::types::AccessResult;

/// Service for comment CRUD operations with tap integration.
///
/// Plugin-optional: instantiated only when the `"comments"` plugin is enabled.
/// Stored as `Option<Arc<CommentService>>` in [`AppState`](crate::state::AppState).
#[derive(Clone)]
pub struct CommentService {
    inner: Arc<CommentServiceInner>,
}

struct CommentServiceInner {
    pool: PgPool,
    dispatcher: Arc<TapDispatcher>,
}

impl CommentService {
    /// Create a new comment service.
    pub fn new(pool: PgPool, dispatcher: Arc<TapDispatcher>) -> Self {
        Self {
            inner: Arc::new(CommentServiceInner { pool, dispatcher }),
        }
    }

    /// Load a comment by ID.
    pub async fn load(&self, id: Uuid) -> Result<Option<Comment>> {
        Comment::find_by_id(&self.inner.pool, id).await
    }

    /// Create a comment with `tap_comment_insert` invocation.
    pub async fn create(&self, input: CreateComment, user: &UserContext) -> Result<Comment> {
        let comment = Comment::create(&self.inner.pool, input).await?;

        let json = serde_json::to_string(&comment).context("serialize comment")?;
        let state = RequestState::without_services(user.clone());
        let _ = self
            .inner
            .dispatcher
            .dispatch("tap_comment_insert", &json, state)
            .await;

        info!(comment_id = %comment.id, item_id = %comment.item_id, "comment created");
        Ok(comment)
    }

    /// Update a comment with `tap_comment_update` invocation.
    pub async fn update(
        &self,
        id: Uuid,
        input: UpdateComment,
        user: &UserContext,
    ) -> Result<Option<Comment>> {
        let comment = Comment::update(&self.inner.pool, id, input).await?;

        if let Some(ref c) = comment {
            let json = serde_json::to_string(c).context("serialize comment")?;
            let state = RequestState::without_services(user.clone());
            let _ = self
                .inner
                .dispatcher
                .dispatch("tap_comment_update", &json, state)
                .await;

            info!(comment_id = %id, "comment updated");
        }

        Ok(comment)
    }

    /// Delete a comment with `tap_comment_delete` invocation (before delete).
    pub async fn delete(&self, id: Uuid, user: &UserContext) -> Result<bool> {
        // Load to dispatch tap before deletion
        if let Some(comment) = self.load(id).await? {
            let json = serde_json::to_string(&comment).context("serialize comment")?;
            let state = RequestState::without_services(user.clone());
            let _ = self
                .inner
                .dispatcher
                .dispatch("tap_comment_delete", &json, state)
                .await;
        }

        let deleted = Comment::delete(&self.inner.pool, id).await?;
        if deleted {
            info!(comment_id = %id, "comment deleted");
        }
        Ok(deleted)
    }

    /// List comments for an item (threaded order).
    pub async fn list_for_item(&self, item_id: Uuid) -> Result<Vec<Comment>> {
        Comment::list_for_item(&self.inner.pool, item_id).await
    }

    /// List all comments (admin moderation).
    pub async fn list_all(&self, limit: i64, offset: i64) -> Result<Vec<Comment>> {
        Comment::list_all(&self.inner.pool, limit, offset).await
    }

    /// List comments by status (admin moderation).
    pub async fn list_by_status(
        &self,
        status: i16,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Comment>> {
        Comment::list_by_status(&self.inner.pool, status, limit, offset).await
    }

    /// Count all comments.
    pub async fn count_all(&self) -> Result<i64> {
        Comment::count_all(&self.inner.pool).await
    }

    /// Check if a user has access to perform an operation on a comment.
    ///
    /// Access control flow:
    /// 1. Admin users always have access
    /// 2. `tap_comment_access` dispatch (Deny/Grant/Neutral aggregation)
    /// 3. Permission fallback based on operation and ownership
    pub async fn check_access(
        &self,
        comment: &Comment,
        operation: &str,
        user: &UserContext,
    ) -> Result<bool> {
        // Admin short-circuit
        if user.is_admin() {
            return Ok(true);
        }

        // Build access check input
        let input = serde_json::json!({
            "comment_id": comment.id,
            "item_id": comment.item_id,
            "author_id": comment.author_id,
            "operation": operation,
            "user_id": user.id,
        });

        let input_json = input.to_string();
        let state = RequestState::without_services(user.clone());

        let results = self
            .inner
            .dispatcher
            .dispatch("tap_comment_access", &input_json, state)
            .await;

        // Aggregate: Deny wins, then Grant, else Neutral
        let mut has_grant = false;
        for result in results {
            if let Ok(access) = serde_json::from_str::<AccessResult>(&result.output) {
                match access {
                    AccessResult::Deny => return Ok(false),
                    AccessResult::Grant => has_grant = true,
                    AccessResult::Neutral => {}
                }
            }
        }

        if has_grant {
            return Ok(true);
        }

        // Permission fallback
        let is_own = comment.author_id == user.id;
        match operation {
            "create" => Ok(user.has_permission("post comments")),
            "edit" => {
                if is_own && user.has_permission("edit own comments") {
                    Ok(true)
                } else {
                    Ok(user.has_permission("edit any comment"))
                }
            }
            "delete" => {
                if is_own && user.has_permission("delete own comments") {
                    Ok(true)
                } else {
                    Ok(user.has_permission("delete any comment"))
                }
            }
            _ => Ok(false),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn comment_access_input_serialization() {
        let input = serde_json::json!({
            "comment_id": Uuid::nil(),
            "item_id": Uuid::nil(),
            "author_id": Uuid::nil(),
            "operation": "edit",
            "user_id": Uuid::nil(),
        });
        let json = input.to_string();
        assert!(json.contains("\"operation\":\"edit\""));
    }
}
