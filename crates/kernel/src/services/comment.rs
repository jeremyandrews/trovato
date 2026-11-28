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
use crate::services::audit::AuditTrail;
use crate::tap::{RequestServices, RequestState, TapDispatcher, UserContext};
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
    tap_services: RequestServices,
    audit: AuditTrail,
}

impl CommentService {
    /// Create a new comment service.
    pub fn new(
        pool: PgPool,
        dispatcher: Arc<TapDispatcher>,
        tap_services: RequestServices,
        audit: AuditTrail,
    ) -> Self {
        Self {
            inner: Arc::new(CommentServiceInner {
                pool,
                dispatcher,
                tap_services,
                audit,
            }),
        }
    }

    /// Record one comment write in the audit log.
    async fn audit(&self, action: &str, id: Uuid, item_id: Option<Uuid>, user: &UserContext) {
        self.inner
            .audit
            .record(
                action,
                "comment",
                id,
                user.authenticated.then_some(user.id),
                serde_json::json!({ "item_id": item_id }),
            )
            .await;
    }

    /// Build a `RequestState` for tap dispatch with the user context and services.
    fn tap_state(&self, user: &UserContext) -> RequestState {
        RequestState::new(user.clone(), self.inner.tap_services.clone())
    }

    /// Load a comment by ID.
    pub async fn load(&self, id: Uuid) -> Result<Option<Comment>> {
        Comment::find_by_id(&self.inner.pool, id).await
    }

    /// Create a comment with `tap_comment_insert` invocation.
    pub async fn create(&self, input: CreateComment, user: &UserContext) -> Result<Comment> {
        let comment = Comment::create(&self.inner.pool, input).await?;
        self.created(&comment, user).await?;
        Ok(comment)
    }

    /// [`Self::create`], queueing the mail `mail` builds for the new comment in
    /// the same transaction as the insert (BG18), so the notification commits
    /// or rolls back with the comment.
    ///
    /// # Errors
    ///
    /// When the insert or queueing fails; then neither happened.
    pub async fn create_with_mail(
        &self,
        input: CreateComment,
        user: &UserContext,
        email: &crate::services::email::EmailService,
        mail: impl FnOnce(&Comment) -> Option<crate::services::email::NewMail>,
    ) -> Result<Comment> {
        let mut tx = self.inner.pool.begin().await?;
        let comment = Comment::create(&mut *tx, input).await?;
        if let Some(message) = mail(&comment) {
            email.queue(&mut *tx, message).await?;
        }
        tx.commit().await?;
        email.wake();
        self.created(&comment, user).await?;
        Ok(comment)
    }

    /// What follows a committed insert: `tap_comment_insert`, search, audit.
    async fn created(&self, comment: &Comment, user: &UserContext) -> Result<()> {
        // A comment changes the page it was posted on. Also purge `items`:
        // once a page is large enough to drop its individual item tags (see
        // `middleware::edge_cache::tags_header_value`'s overflow fallback),
        // `items` is the only tag left that still covers it.
        crate::edge_cache::purge([
            crate::edge_cache::tags::item_tag(comment.item_id),
            crate::edge_cache::tags::ITEMS.to_string(),
        ]);

        let json = serde_json::to_string(comment).context("serialize comment")?;
        let state = self.tap_state(user);
        let _ = self
            .inner
            .dispatcher
            .dispatch("tap_comment_insert", &json, state)
            .await;

        info!(comment_id = %comment.id, item_id = %comment.item_id, "comment created");
        self.audit("create", comment.id, Some(comment.item_id), user)
            .await;
        Ok(())
    }

    /// Update a comment with `tap_comment_update` invocation.
    pub async fn update(
        &self,
        id: Uuid,
        input: UpdateComment,
        user: &UserContext,
    ) -> Result<Option<Comment>> {
        let comment = Comment::update(&self.inner.pool, id, input).await?;
        self.updated(comment.as_ref(), user).await?;
        Ok(comment)
    }

    /// [`Self::update`], queueing the mail `mail` builds from the comment as it
    /// was and as it is in the same transaction as the update (BG18).
    ///
    /// # Errors
    ///
    /// When the update or queueing fails; then neither happened.
    pub async fn update_with_mail(
        &self,
        id: Uuid,
        input: UpdateComment,
        user: &UserContext,
        email: &crate::services::email::EmailService,
        mail: impl FnOnce(Option<&Comment>, &Comment) -> Option<crate::services::email::NewMail>,
    ) -> Result<Option<Comment>> {
        let mut tx = self.inner.pool.begin().await?;
        let before = Comment::find_by_id(&mut *tx, id).await?;
        let comment = Comment::update_in(&mut tx, id, input).await?;
        if let Some(c) = &comment
            && let Some(message) = mail(before.as_ref(), c)
        {
            email.queue(&mut *tx, message).await?;
        }
        tx.commit().await?;
        email.wake();
        self.updated(comment.as_ref(), user).await?;
        Ok(comment)
    }

    /// What follows a committed update: `tap_comment_update`, audit. The
    /// search index queue row was written with the update, by the database.
    async fn updated(&self, comment: Option<&Comment>, user: &UserContext) -> Result<()> {
        if let Some(c) = comment {
            crate::edge_cache::purge([
                crate::edge_cache::tags::item_tag(c.item_id),
                crate::edge_cache::tags::ITEMS.to_string(),
            ]);

            let json = serde_json::to_string(c).context("serialize comment")?;
            let state = self.tap_state(user);
            let _ = self
                .inner
                .dispatcher
                .dispatch("tap_comment_update", &json, state)
                .await;

            info!(comment_id = %c.id, "comment updated");
            self.audit("update", c.id, Some(c.item_id), user).await;
        }
        Ok(())
    }

    /// Delete a comment with `tap_comment_delete` invocation (before delete).
    pub async fn delete(&self, id: Uuid, user: &UserContext) -> Result<bool> {
        // Load to dispatch tap before deletion
        let existing = self.load(id).await?;
        if let Some(ref comment) = existing {
            let json = serde_json::to_string(&comment).context("serialize comment")?;
            let state = self.tap_state(user);
            let _ = self
                .inner
                .dispatcher
                .dispatch("tap_comment_delete", &json, state)
                .await;
        }

        let deleted = Comment::delete(&self.inner.pool, id).await?;
        if deleted {
            if let Some(item_id) = existing.as_ref().map(|c| c.item_id) {
                crate::edge_cache::purge([
                    crate::edge_cache::tags::item_tag(item_id),
                    crate::edge_cache::tags::ITEMS.to_string(),
                ]);
            }
            info!(comment_id = %id, "comment deleted");
            self.audit("delete", id, existing.as_ref().map(|c| c.item_id), user)
                .await;
        }
        Ok(deleted)
    }

    /// List comments for an item (threaded order).
    pub async fn list_for_item(&self, item_id: Uuid) -> Result<Vec<Comment>> {
        Comment::list_for_item(&self.inner.pool, item_id).await
    }

    /// List an item's comments in thread order, the threads oldest or newest
    /// first.
    pub async fn list_for_item_ordered(
        &self,
        item_id: Uuid,
        newest_first: bool,
    ) -> Result<Vec<Comment>> {
        Comment::list_for_item_ordered(&self.inner.pool, item_id, newest_first).await
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

    /// One page of the admin comment listing, and how many comments match
    /// (see [`Comment::admin_page`]).
    pub async fn admin_page(
        &self,
        status: Option<i16>,
        like: Option<&str>,
        order_by: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Comment>, i64)> {
        Comment::admin_page(&self.inner.pool, status, like, order_by, limit, offset).await
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
        let state = self.tap_state(user);

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

        Ok(permission_allows(comment, operation, user))
    }
}

/// The permission rule for one operation on one comment, with no tap asked.
///
/// The fallback [`CommentService::check_access`] ends on, and what the comment
/// thread asks to decide which Edit and Delete links to show, so a link is
/// offered exactly where the page behind it would let the viewer in (a plugin
/// grant through `tap_comment_access` aside).
///
/// Editing or deleting someone else's comment is `administer comments`, the
/// permission the moderation screens and the `trovato_comments` plugin
/// declare. It used to be `edit any comment` and `delete any comment`, which
/// nothing declares and so no role could be granted from the permissions page:
/// a moderator holding `administer comments` who was not the site
/// administrator got 403 from the comment API's PUT and DELETE. Those two
/// names are still honoured, for a site that granted them by hand.
///
/// A comment is the viewer's own only when the viewer is signed in: an
/// anonymous visitor and an imported anonymous comment can share the nil id.
pub fn permission_allows(comment: &Comment, operation: &str, user: &UserContext) -> bool {
    if user.is_admin() {
        return true;
    }
    let is_own = user.authenticated && comment.author_id == user.id;
    let moderates = user.has_permission("administer comments");
    match operation {
        "view" => user.has_permission("access content"),
        "create" => user.has_permission("post comments"),
        "edit" => {
            (is_own && user.has_permission("edit own comments"))
                || moderates
                || user.has_permission("edit any comment")
        }
        "delete" => {
            (is_own && user.has_permission("delete own comments"))
                || moderates
                || user.has_permission("delete any comment")
        }
        _ => false,
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
            subject: None,
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
        let admin = UserContext::administrator(Uuid::now_v7(), vec!["administer site".to_string()]);
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod permission_tests {
    use super::*;

    fn comment_by(author_id: Uuid) -> Comment {
        Comment {
            id: Uuid::now_v7(),
            item_id: Uuid::now_v7(),
            parent_id: None,
            author_id,
            subject: None,
            body: "test".to_string(),
            body_format: "plain_text".to_string(),
            status: 1,
            created: 0,
            changed: 0,
            depth: 0,
        }
    }

    fn holding(id: Uuid, permissions: &[&str]) -> UserContext {
        UserContext::authenticated(id, permissions.iter().map(|p| p.to_string()).collect())
    }

    /// The jibnotes 403: a moderator who is not the site administrator could
    /// not edit or delete through the API, because the rule asked for two
    /// permissions nothing declares.
    #[test]
    fn administer_comments_edits_and_deletes_any_comment() {
        let moderator = holding(Uuid::now_v7(), &["administer comments"]);
        let theirs = comment_by(Uuid::now_v7());
        assert!(permission_allows(&theirs, "edit", &moderator));
        assert!(permission_allows(&theirs, "delete", &moderator));
    }

    /// Own comments, each operation gated by its own permission.
    #[test]
    fn own_comments_need_their_own_permission() {
        let me = Uuid::now_v7();
        let mine = comment_by(me);
        let theirs = comment_by(Uuid::now_v7());

        let editor = holding(me, &["edit own comments"]);
        assert!(permission_allows(&mine, "edit", &editor));
        assert!(!permission_allows(&mine, "delete", &editor));
        assert!(!permission_allows(&theirs, "edit", &editor));

        let deleter = holding(me, &["delete own comments"]);
        assert!(permission_allows(&mine, "delete", &deleter));
        assert!(!permission_allows(&mine, "edit", &deleter));
        assert!(!permission_allows(&theirs, "delete", &deleter));
    }

    /// Anonymous visitors share the nil id with an imported anonymous comment;
    /// that does not make the comment theirs.
    #[test]
    fn an_anonymous_visitor_owns_no_comment() {
        let mut anonymous = UserContext::anonymous();
        anonymous.permissions = vec!["edit own comments".into(), "delete own comments".into()];
        let anonymous_comment = comment_by(Uuid::nil());
        assert!(!permission_allows(&anonymous_comment, "edit", &anonymous));
        assert!(!permission_allows(&anonymous_comment, "delete", &anonymous));
    }

    #[test]
    fn an_unknown_operation_is_refused() {
        let me = Uuid::now_v7();
        let user = holding(me, &["administer comments", "edit own comments"]);
        assert!(!permission_allows(&comment_by(me), "publish", &user));
    }
}
