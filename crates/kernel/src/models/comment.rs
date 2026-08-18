//! Comment model for threaded discussions on content items.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Publication status of a comment.
///
/// Stored as the `smallint` `comment.status`. Two values existed before
/// moderation did — 0 unpublished, 1 published — and every new comment was
/// created as 1, so there was no way to hold one for review. The admin list
/// even labelled 0 as "Pending", which is what a moderator would call a comment
/// awaiting review rather than one they had already hidden.
///
/// Only [`Self::Published`] is publicly visible. Anything this enum does not
/// recognise is treated as invisible by [`Self::from_i16`], so an unknown value
/// in the column fails closed rather than being shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentStatus {
    /// Hidden by a moderator after the fact. Value 0.
    Unpublished,
    /// Visible on the site. Value 1.
    Published,
    /// Awaiting moderation, never shown. Value 2.
    Pending,
    /// Classified as spam. Never shown, kept rather than deleted so a false
    /// positive can be recovered and so a classifier has something to learn
    /// from. Value 3.
    Spam,
}

impl CommentStatus {
    /// The stored column value.
    pub fn as_i16(self) -> i16 {
        match self {
            Self::Unpublished => 0,
            Self::Published => 1,
            Self::Pending => 2,
            Self::Spam => 3,
        }
    }

    /// Read a stored column value.
    ///
    /// `None` for a value this build does not know, which callers must treat as
    /// not visible: a column written by a newer version must not become
    /// published by accident.
    pub fn from_i16(value: i16) -> Option<Self> {
        match value {
            0 => Some(Self::Unpublished),
            1 => Some(Self::Published),
            2 => Some(Self::Pending),
            3 => Some(Self::Spam),
            _ => None,
        }
    }

    /// Whether a comment with this status is shown to the public.
    pub fn is_visible(self) -> bool {
        matches!(self, Self::Published)
    }

    /// Whether this status is one a moderation queue should surface.
    pub fn awaits_review(self) -> bool {
        matches!(self, Self::Pending)
    }

    /// Human-readable label, used by the admin screens.
    pub fn label(self) -> &'static str {
        match self {
            Self::Unpublished => "Unpublished",
            Self::Published => "Published",
            Self::Pending => "Pending review",
            Self::Spam => "Spam",
        }
    }

    /// CSS class suffix for the admin status badge.
    pub fn css_suffix(self) -> &'static str {
        match self {
            Self::Unpublished => "unpublished",
            Self::Published => "published",
            Self::Pending => "pending",
            Self::Spam => "spam",
        }
    }

    /// The status a newly posted comment gets, from the `comment_default_status`
    /// site setting.
    ///
    /// Only `published` and `pending` are meaningful answers, so those are the
    /// only two accepted. Unset means [`Self::Published`], which is what every
    /// comment did before this setting existed — upgrading a site must not
    /// silently start holding its comments.
    ///
    /// A value that is set but unrecognised resolves to [`Self::Pending`], which
    /// is the recoverable direction: a comment wrongly held is sitting in a
    /// queue, while a comment wrongly published is already on the site.
    pub async fn default_for_new_comments(pool: &PgPool) -> Self {
        let configured = crate::models::SiteConfig::get(pool, DEFAULT_STATUS_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(str::to_string));

        match configured.as_deref() {
            None => Self::Published,
            Some("published") => Self::Published,
            Some("pending") => Self::Pending,
            Some(other) => {
                tracing::warn!(
                    value = %other,
                    "unrecognised {DEFAULT_STATUS_KEY}; holding new comments for review"
                );
                Self::Pending
            }
        }
    }

    /// The status a new comment gets, given everything that can excuse it from the
    /// review queue.
    ///
    /// Pure, so the precedence is testable without a database:
    ///
    /// 1. If the site publishes immediately, nothing else matters.
    /// 2. `skip comment approval` (or admin) bypasses the queue.
    /// 3. Otherwise an author with at least `threshold` approved comments bypasses
    ///    it too — the trust ladder. `None` means the ladder is off.
    ///
    /// The ladder only ever *promotes* out of pending. It cannot publish a comment
    /// on a site that holds nothing, and it cannot hold one on a site that
    /// publishes everything.
    pub fn for_new_comment(
        site_default: Self,
        may_skip_approval: bool,
        approved_comments: i64,
        threshold: Option<i64>,
    ) -> Self {
        if !site_default.awaits_review() {
            return site_default;
        }

        if may_skip_approval {
            return Self::Published;
        }

        match threshold {
            Some(threshold) if approved_comments >= threshold => Self::Published,
            _ => site_default,
        }
    }

    /// How many approved comments an author needs to skip the queue.
    ///
    /// `None` when the ladder is disabled: an explicit `0`, or a value that is not
    /// a positive number. A ladder nobody can parse should not hand out bypasses.
    pub async fn trust_threshold(pool: &PgPool) -> Option<i64> {
        let configured = crate::models::SiteConfig::get(pool, TRUST_THRESHOLD_KEY)
            .await
            .ok()
            .flatten();

        let threshold = match configured {
            None => DEFAULT_TRUST_THRESHOLD,
            Some(value) => match value.as_i64().or_else(|| value.as_str()?.parse().ok()) {
                Some(parsed) => parsed,
                None => {
                    tracing::warn!(
                        value = %value,
                        "unrecognised {TRUST_THRESHOLD_KEY}; disabling the trust ladder"
                    );
                    return None;
                }
            },
        };

        (threshold > 0).then_some(threshold)
    }
}

/// Site setting naming the status new comments are created with.
pub const DEFAULT_STATUS_KEY: &str = "comment_default_status";

/// Site setting naming how many approved comments earn an author the queue
/// bypass. `0` disables the ladder.
pub const TRUST_THRESHOLD_KEY: &str = "comment_trust_threshold";

/// Default trust threshold: three approved comments.
///
/// Low on purpose. The ladder is not a security boundary — the classifier still
/// runs on every comment and can take a published one down — it is a latency
/// exemption for people who have already been read by a human three times.
pub const DEFAULT_TRUST_THRESHOLD: i64 = 3;

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

    /// Publication status. See [`CommentStatus`], which this is the stored
    /// form of.
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
        let status = input
            .status
            .unwrap_or_else(|| CommentStatus::Published.as_i16());

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
                WHERE item_id = $1 AND parent_id IS NULL AND status = $2

                UNION ALL

                -- Recursive case: replies
                SELECT c.id, c.item_id, c.parent_id, c.author_id, c.body, c.body_format, c.status, c.created, c.changed, c.depth,
                       ct.sort_path || c.created
                FROM comment c
                JOIN comment_tree ct ON c.parent_id = ct.id
                WHERE c.status = $2
            )
            SELECT id, item_id, parent_id, author_id, body, body_format, status, created, changed, depth
            FROM comment_tree
            ORDER BY sort_path
            "#,
        )
        .bind(item_id)
        .bind(CommentStatus::Published.as_i16())
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
            WHERE item_id = $1 AND status = $4
            ORDER BY created DESC
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(item_id)
        .bind(limit)
        .bind(offset)
        .bind(CommentStatus::Published.as_i16())
        .fetch_all(pool)
        .await
        .context("failed to list comments for item")?;

        Ok(comments)
    }

    /// Count comments for an item.
    pub async fn count_for_item(pool: &PgPool, item_id: Uuid) -> Result<i64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM comment WHERE item_id = $1 AND status = $2")
                .bind(item_id)
                .bind(CommentStatus::Published.as_i16())
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

    /// How many published comments an author has.
    ///
    /// The trust ladder's input. Counts published only: a pending, hidden or spam
    /// comment is not evidence of anything, which is what stops a spammer from
    /// earning trust by posting into the queue.
    pub async fn approved_count_for_author(pool: &PgPool, author_id: Uuid) -> Result<i64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM comment WHERE author_id = $1 AND status = $2")
                .bind(author_id)
                .bind(CommentStatus::Published.as_i16())
                .fetch_one(pool)
                .await
                .context("failed to count approved comments for author")?;

        Ok(count)
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
        let Some(existing) = Self::find_by_id(pool, id).await? else {
            return Ok(None);
        };

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
            WHERE parent_id = $1 AND status = $2
            ORDER BY created ASC
            "#,
        )
        .bind(comment_id)
        .bind(CommentStatus::Published.as_i16())
        .fetch_all(pool)
        .await
        .context("failed to get replies")?;

        Ok(comments)
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The stored values are a wire format: existing rows must keep meaning what
    /// they meant, so 0 and 1 are pinned rather than left to declaration order.
    #[test]
    fn the_stored_values_are_stable() {
        assert_eq!(CommentStatus::Unpublished.as_i16(), 0);
        assert_eq!(CommentStatus::Published.as_i16(), 1);
        assert_eq!(CommentStatus::Pending.as_i16(), 2);
        assert_eq!(CommentStatus::Spam.as_i16(), 3);
    }

    #[test]
    fn every_status_round_trips_through_its_stored_value() {
        for status in [
            CommentStatus::Unpublished,
            CommentStatus::Published,
            CommentStatus::Pending,
            CommentStatus::Spam,
        ] {
            assert_eq!(CommentStatus::from_i16(status.as_i16()), Some(status));
        }
    }

    /// A value from a newer version must not be guessed at, and callers treat
    /// `None` as not visible.
    #[test]
    fn an_unknown_stored_value_is_not_a_status() {
        for value in [-1, 4, 99] {
            assert_eq!(CommentStatus::from_i16(value), None, "value {value}");
        }
    }

    #[test]
    fn only_published_is_visible() {
        assert!(CommentStatus::Published.is_visible());
        for hidden in [
            CommentStatus::Unpublished,
            CommentStatus::Pending,
            CommentStatus::Spam,
        ] {
            assert!(!hidden.is_visible(), "{hidden:?} must not be visible");
        }
    }

    /// Only pending is a queue: an unpublished comment was decided on, and spam
    /// was too.
    #[test]
    fn only_pending_awaits_review() {
        assert!(CommentStatus::Pending.awaits_review());
        for decided in [
            CommentStatus::Unpublished,
            CommentStatus::Published,
            CommentStatus::Spam,
        ] {
            assert!(!decided.awaits_review(), "{decided:?}");
        }
    }

    /// A site that publishes immediately is unaffected by the ladder: it cannot
    /// hold a comment that the site would have published.
    #[test]
    fn the_ladder_cannot_hold_a_comment_on_an_open_site() {
        for approved in [0, 100] {
            assert_eq!(
                CommentStatus::for_new_comment(CommentStatus::Published, false, approved, Some(3)),
                CommentStatus::Published
            );
        }
    }

    /// A new account waits for classification. This is the case the ladder exists
    /// to distinguish.
    #[test]
    fn a_new_account_waits_in_the_queue() {
        assert_eq!(
            CommentStatus::for_new_comment(CommentStatus::Pending, false, 0, Some(3)),
            CommentStatus::Pending
        );
        assert_eq!(
            CommentStatus::for_new_comment(CommentStatus::Pending, false, 2, Some(3)),
            CommentStatus::Pending,
            "one short of the threshold is still short"
        );
    }

    /// An account that has been read by a human enough times skips the wait.
    #[test]
    fn an_established_account_skips_the_queue() {
        assert_eq!(
            CommentStatus::for_new_comment(CommentStatus::Pending, false, 3, Some(3)),
            CommentStatus::Published,
            "the threshold is inclusive"
        );
        assert_eq!(
            CommentStatus::for_new_comment(CommentStatus::Pending, false, 50, Some(3)),
            CommentStatus::Published
        );
    }

    /// With the ladder off, only the explicit permission excuses anyone.
    #[test]
    fn a_disabled_ladder_grants_nothing() {
        assert_eq!(
            CommentStatus::for_new_comment(CommentStatus::Pending, false, 1000, None),
            CommentStatus::Pending
        );
        assert_eq!(
            CommentStatus::for_new_comment(CommentStatus::Pending, true, 0, None),
            CommentStatus::Published,
            "the permission still works with the ladder off"
        );
    }

    /// The explicit grant does not depend on any history.
    #[test]
    fn the_skip_permission_does_not_need_a_history() {
        assert_eq!(
            CommentStatus::for_new_comment(CommentStatus::Pending, true, 0, Some(3)),
            CommentStatus::Published
        );
    }

    /// The ladder only promotes out of pending; it never moves a comment into a
    /// non-visible status.
    #[test]
    fn the_ladder_only_ever_promotes() {
        for site_default in [
            CommentStatus::Published,
            CommentStatus::Unpublished,
            CommentStatus::Spam,
        ] {
            assert_eq!(
                CommentStatus::for_new_comment(site_default, false, 100, Some(3)),
                site_default,
                "{site_default:?} is not a review queue, so the ladder must not touch it"
            );
        }
    }

    /// The admin list used to label status 0 "Pending", which is what a
    /// moderator calls a comment awaiting review rather than one they hid.
    #[test]
    fn unpublished_and_pending_have_distinct_labels() {
        assert_eq!(CommentStatus::Unpublished.label(), "Unpublished");
        assert_eq!(CommentStatus::Pending.label(), "Pending review");
        assert_ne!(
            CommentStatus::Unpublished.css_suffix(),
            CommentStatus::Pending.css_suffix()
        );
    }
}
