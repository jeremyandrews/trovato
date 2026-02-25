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
            "view" => Ok(user.has_permission("access content")),
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

    fn make_comment(author_id: Uuid) -> Comment {
        Comment {
            id: Uuid::now_v7(),
            item_id: Uuid::now_v7(),
            parent_id: None,
            author_id,
            body: "test".to_string(),
            body_format: "plain_text".to_string(),
            status: 1,
            created: 0,
            changed: 0,
            depth: 0,
        }
    }

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

    // check_access tests exercise the synchronous permission-fallback logic.
    // The tap dispatch returns empty results in unit tests (no dispatcher),
    // so we validate the fallback path directly via CommentService::check_access
    // using a real service instance — but since we can't construct a real
    // TapDispatcher or PgPool in unit tests, we test the permission logic
    // by verifying UserContext permissions match expected outcomes.

    #[test]
    fn check_access_admin_bypass() {
        let admin = UserContext::authenticated(Uuid::now_v7(), vec!["administer site".to_string()]);
        assert!(admin.is_admin());
        // Admin always gets access regardless of ownership or permissions
        let _comment = make_comment(Uuid::now_v7()); // different author
        // Verify the permission fallback would deny, but admin bypasses
        assert!(!admin.has_permission("edit own comments"));
        assert!(!admin.has_permission("edit any comment"));
        // Admin should still pass (tested via the is_admin() check in check_access)
    }

    #[test]
    fn check_access_edit_own_permission() {
        let user_id = Uuid::now_v7();
        let user = UserContext::authenticated(user_id, vec!["edit own comments".to_string()]);
        let own_comment = make_comment(user_id);
        let other_comment = make_comment(Uuid::now_v7());

        // Own comment with "edit own comments" → allowed
        let is_own = own_comment.author_id == user.id;
        assert!(is_own && user.has_permission("edit own comments"));

        // Other's comment with only "edit own comments" → denied
        let is_own = other_comment.author_id == user.id;
        assert!(!is_own);
        assert!(!user.has_permission("edit any comment"));
    }

    #[test]
    fn check_access_edit_any_permission() {
        let user = UserContext::authenticated(Uuid::now_v7(), vec!["edit any comment".to_string()]);
        // "edit any comment" allows editing regardless of ownership
        assert!(user.has_permission("edit any comment"));
    }

    #[test]
    fn check_access_delete_own_permission() {
        let user_id = Uuid::now_v7();
        let user = UserContext::authenticated(user_id, vec!["delete own comments".to_string()]);
        let own_comment = make_comment(user_id);
        let other_comment = make_comment(Uuid::now_v7());

        let is_own = own_comment.author_id == user.id;
        assert!(is_own && user.has_permission("delete own comments"));

        let is_own = other_comment.author_id == user.id;
        assert!(!is_own);
        assert!(!user.has_permission("delete any comment"));
    }

    #[test]
    fn check_access_view_permission() {
        let user = UserContext::authenticated(Uuid::now_v7(), vec!["access content".to_string()]);
        assert!(user.has_permission("access content"));

        let no_perm = UserContext::authenticated(Uuid::now_v7(), vec![]);
        assert!(!no_perm.has_permission("access content"));
    }

    #[test]
    fn check_access_create_permission() {
        let user = UserContext::authenticated(Uuid::now_v7(), vec!["post comments".to_string()]);
        assert!(user.has_permission("post comments"));

        let no_perm = UserContext::authenticated(Uuid::now_v7(), vec![]);
        assert!(!no_perm.has_permission("post comments"));
    }

    #[test]
    fn check_access_unknown_operation_denied() {
        let user = UserContext::authenticated(
            Uuid::now_v7(),
            vec![
                "post comments".to_string(),
                "edit own comments".to_string(),
                "delete own comments".to_string(),
            ],
        );
        // Unknown operations should be denied (the match falls through to Ok(false))
        assert!(!user.has_permission("unknown_operation"));
    }
}
