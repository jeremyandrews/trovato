//! Item model and CRUD operations.
//!
//! Items are the core content records in Trovato (like nodes in Drupal).
//! They support JSONB field storage and revision history.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, PgPool};
use uuid::Uuid;

use super::stage::LIVE_STAGE_ID;

/// Item record (content record).
///
/// SYNC: field names and types must match `crates/plugin-sdk/src/types.rs` Item.
/// The kernel serializes this struct via `serde_json::to_string()` for tap dispatch.
/// The SDK deserializes it. Extra kernel-only fields (promote, sticky, language,
/// item_group_id) are safely ignored by the SDK's serde.
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

    /// Stage UUID referencing category_tag(id) in the "stages" category.
    pub stage_id: Uuid,

    /// Language code (default: 'en').
    pub language: String,

    /// Group ID linking copies of the same logical item across stages.
    pub item_group_id: Uuid,

    /// Data retention period in days (NULL = keep indefinitely).
    #[serde(default)]
    pub retention_days: Option<i32>,
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

    /// Reserved for a structured change summary. **Nothing writes it yet**:
    /// neither `INSERT INTO item_revision` names the column, so it is NULL on
    /// every revision. A comparison of two revisions is computed from their
    /// `fields` snapshots (`content::revision_compare`), not read from here.
    #[serde(default)]
    pub change_summary: Option<serde_json::Value>,

    /// Reserved for "AI was involved in this revision". **Nothing writes it
    /// yet**: every revision carries the column default.
    #[serde(default)]
    pub ai_generated: Option<bool>,

    /// The content moderation state of this revision, `draft`, `published` or
    /// `archived`, on a moderated type (`content::moderation`). `None` on a
    /// revision saved while the type was not moderated, which reads as
    /// published or draft from its `status`.
    #[serde(default)]
    #[sqlx(default)]
    pub moderation_state: Option<String>,
}

/// One row of the moderated content list ([`Item::list_moderated`]): an item
/// and its latest revision.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ModeratedItem {
    /// The item.
    pub id: Uuid,
    /// Its content type.
    pub item_type: String,
    /// The latest revision's title.
    pub title: String,
    /// Who saved the latest revision.
    pub author_id: Uuid,
    /// When the latest revision was saved, a Unix timestamp.
    pub changed: i64,
    /// The latest revision's state: `draft` or `archived`.
    pub state: String,
    /// Whether the latest revision is a draft ahead of the published one.
    pub ahead: bool,
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
    pub stage_id: Option<Uuid>,
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
    /// Move the item to this stage. `None` leaves it where it is.
    ///
    /// The model moves it, with its URL aliases and menu links, in the same
    /// transaction as the revision, and decides nothing: whether this user may
    /// make the move is `ItemService::update`'s question.
    #[serde(default)]
    pub stage_id: Option<Uuid>,
    /// Give the item this author. `None` leaves it.
    ///
    /// Only a holder of `administer content` may change it; the model stores
    /// what it is given, and `ItemService::update` decides (B107).
    #[serde(default)]
    pub author_id: Option<Uuid>,
    /// Set the item's authored on time, a Unix timestamp. `None` leaves it.
    /// Decided as [`Self::author_id`] is.
    #[serde(default)]
    pub created: Option<i64>,
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
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language, item_group_id, retention_days FROM item WHERE id = $1"
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch item by id")?;

        Ok(item)
    }

    /// Find every item whose id is in `ids`, in one query. Missing ids are
    /// simply absent from the result.
    pub async fn find_by_ids(pool: &PgPool, ids: &[Uuid]) -> Result<Vec<Self>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, Item>(
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language, item_group_id, retention_days FROM item WHERE id = ANY($1)"
        )
        .bind(ids)
        .fetch_all(pool)
        .await
        .context("failed to fetch items by id")
    }

    /// List items by content type.
    pub async fn list_by_type(pool: &PgPool, item_type: &str) -> Result<Vec<Self>> {
        let items = sqlx::query_as::<_, Item>(
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language, item_group_id, retention_days FROM item WHERE type = $1 ORDER BY created DESC"
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
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language, item_group_id, retention_days FROM item WHERE author_id = $1 ORDER BY created DESC"
        )
        .bind(author_id)
        .fetch_all(pool)
        .await
        .context("failed to list items by author")?;

        Ok(items)
    }

    /// How many items an author has, at any status and on any stage.
    pub async fn count_by_author(pool: &PgPool, author_id: Uuid) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE author_id = $1")
            .bind(author_id)
            .fetch_one(pool)
            .await
            .context("failed to count items by author")?;

        Ok(count)
    }

    /// List published items (live stage only).
    pub async fn list_published(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<Self>> {
        let items = sqlx::query_as::<_, Item>(
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language, item_group_id, retention_days FROM item WHERE status = 1 AND stage_id = $1 ORDER BY sticky DESC, created DESC LIMIT $2 OFFSET $3"
        )
        .bind(LIVE_STAGE_ID)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .context("failed to list published items")?;

        Ok(items)
    }

    /// List published items promoted to the front page (live stage only).
    ///
    /// Selects on `promote` in SQL rather than filtering a page of published
    /// items in memory. Filtering a fixed page is wrong at any page size: a
    /// promoted item that sits behind that many newer published items is
    /// simply never seen, however many promoted items the site has.
    pub async fn list_promoted(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<Self>> {
        let items = sqlx::query_as::<_, Item>(
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language, item_group_id, retention_days FROM item WHERE status = 1 AND promote = 1 AND stage_id = $1 ORDER BY sticky DESC, created DESC LIMIT $2 OFFSET $3"
        )
        .bind(LIVE_STAGE_ID)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .context("failed to list promoted items")?;

        Ok(items)
    }

    /// Create a new item with initial revision.
    pub async fn create(pool: &PgPool, input: CreateItem) -> Result<Self> {
        Self::create_with_file_refs(pool, input, None, None, None).await
    }

    /// [`Self::create`], also writing the item's file references (and promoting
    /// the files they name) in the same transaction when `refs` is given. See
    /// [`crate::content::file_refs`]. `created` sets the authored on time;
    /// `None` is now.
    pub(crate) async fn create_with_file_refs(
        pool: &PgPool,
        input: CreateItem,
        refs: Option<&crate::content::file_refs::FileReferences>,
        created: Option<i64>,
        moderation_state: Option<&str>,
    ) -> Result<Self> {
        let now = chrono::Utc::now().timestamp();
        let item_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();

        // Start a transaction
        let mut tx = pool.begin().await.context("failed to start transaction")?;

        // Insert item (without current_revision_id first)
        sqlx::query(
            r#"
            INSERT INTO item (id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language, item_group_id)
            VALUES ($1, NULL, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                    COALESCE($11, (SELECT tag_id FROM stage_config WHERE is_default), $14),
                    $12, $13)
            "#,
        )
        .bind(item_id)
        .bind(&input.item_type)
        .bind(&input.title)
        .bind(input.author_id)
        .bind(input.status.unwrap_or(1))
        .bind(created.unwrap_or(now))
        .bind(now)
        .bind(input.promote.unwrap_or(0))
        .bind(input.sticky.unwrap_or(0))
        .bind(input.fields.clone().unwrap_or(serde_json::json!({})))
        // No stage given: the stage marked default, resolved inside this one
        // INSERT (no extra round trip), and Live if somehow none is marked.
        // `uq_stage_is_default` keeps the subquery to one row.
        .bind(input.stage_id)
        // Routes should always provide the resolved language; "en" is a last-resort safety net.
        .bind(input.language.as_deref().unwrap_or("en"))
        // New items are their own group (item_group_id = item_id)
        .bind(item_id)
        .bind(LIVE_STAGE_ID)
        .execute(&mut *tx)
        .await
        .context("failed to insert item")?;

        // Insert initial revision
        sqlx::query(
            r#"
            INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, log, moderation_state)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
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
        .bind(moderation_state)
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

        if let Some(refs) = refs {
            crate::content::file_refs::sync_item_file_references(&mut tx, item_id, refs).await?;
        }

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
        Self::update_with_file_refs(pool, id, author_id, input, None, None).await
    }

    /// [`Self::update`], also rewriting the item's file references (and
    /// promoting the files they name) in the same transaction when `refs` is
    /// given. See [`crate::content::file_refs`].
    pub(crate) async fn update_with_file_refs(
        pool: &PgPool,
        id: Uuid,
        author_id: Uuid,
        input: UpdateItem,
        refs: Option<&crate::content::file_refs::FileReferences>,
        moderation_state: Option<&str>,
    ) -> Result<Option<Self>> {
        let now = chrono::Utc::now().timestamp();
        let revision_id = Uuid::now_v7();

        // Fetch current item
        let Some(current) = Self::find_by_id(pool, id).await? else {
            return Ok(None);
        };

        // Each top level value the input leaves `None` keeps the stored one.
        // `fields` is replaced whole when given: the caller has already built
        // the complete map (a form save starts from the stored fields, see
        // `content::submission::build_item_fields`).
        let title = input.title.unwrap_or(current.title);
        let status = input.status.unwrap_or(current.status);
        let promote = input.promote.unwrap_or(current.promote);
        let sticky = input.sticky.unwrap_or(current.sticky);
        let fields = input.fields.unwrap_or(current.fields);
        let log = input.log;
        let stage_id = input.stage_id.unwrap_or(current.stage_id);
        let item_author = input.author_id.unwrap_or(current.author_id);
        let created = input.created.unwrap_or(current.created);

        // Start transaction
        let mut tx = pool.begin().await.context("failed to start transaction")?;

        // Create new revision FIRST (before updating item to point to it)
        sqlx::query(
            r#"
            INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, log, moderation_state)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
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
        .bind(moderation_state)
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
                current_revision_id = $7,
                stage_id = $9,
                author_id = $10,
                created = $11
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
        .bind(stage_id)
        .bind(item_author)
        .bind(created)
        .execute(&mut *tx)
        .await
        .context("failed to update item")?;

        // A stage move takes the item's URL aliases and menu links on the old
        // stage with it, in this transaction, through the stage publish code
        // narrowed to this item.
        if stage_id != current.stage_id {
            crate::stage::move_dependents(&mut tx, current.stage_id, stage_id, Some(id)).await?;
        }

        if let Some(refs) = refs {
            crate::content::file_refs::sync_item_file_references(&mut tx, id, refs).await?;
        }

        tx.commit().await.context("failed to commit transaction")?;
        // A menu label mirrors its target's title, and a stage move took the
        // item's aliases and menu links with it.
        crate::menu::invalidate_config_menus();

        // Return updated item
        Self::find_by_id(pool, id).await
    }

    /// Delete an item and all its revisions.
    pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool> {
        let mut tx = pool.begin().await.context("failed to start item delete")?;
        let deleted = Self::delete_in(&mut tx, id).await?;
        if !deleted {
            // No item: leave whatever else had its path alone.
            tx.rollback()
                .await
                .context("failed to roll back item delete")?;
            return Ok(false);
        }
        tx.commit().await.context("failed to commit item delete")?;
        crate::menu::invalidate_config_menus();

        Ok(deleted)
    }

    /// Delete an item inside the caller's transaction, with what points at it:
    /// its URL aliases, the menu links to its `/item/{id}` path or to any of
    /// those aliases, and its queued kernel embed jobs. Revisions and file
    /// references go by `ON DELETE CASCADE`. The caller invalidates the
    /// navigation cache once the transaction commits.
    pub async fn delete_in(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        id: Uuid,
    ) -> Result<bool> {
        let source = format!("/item/{id}");
        let mut paths: Vec<String> =
            sqlx::query_scalar("SELECT alias FROM url_alias WHERE source = $1")
                .bind(&source)
                .fetch_all(&mut **tx)
                .await
                .context("failed to load item aliases")?;
        paths.push(source.clone());

        crate::models::MenuLink::delete_by_paths_in(tx, &paths).await?;
        crate::models::UrlAlias::delete_by_source_in(tx, &source).await?;

        let result = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(id)
            .execute(&mut **tx)
            .await
            .context("failed to delete item")?;

        // The item's queued kernel embed jobs: `plugin_queue` has no FK to the
        // item (the id lives in the JSON payload), so nothing cascades. Only the
        // kernel embed identity is touched; other plugins' rows are theirs.
        //
        // After the item row, not before: an enqueue holds `FOR KEY SHARE` on
        // the item until its job commits (`embed_index::enqueue_embed_job`), so
        // the item delete above waited for any such job, and this statement,
        // reading afresh, sees and removes it.
        sqlx::query("DELETE FROM plugin_queue WHERE plugin_name = $1 AND payload->>'item_id' = $2")
            .bind(crate::services::embed_index::KERNEL_EMBED_PLUGIN)
            .bind(id.to_string())
            .execute(&mut **tx)
            .await
            .context("failed to delete item embed jobs")?;

        Ok(result.rows_affected() > 0)
    }

    /// Get all revisions for an item.
    pub async fn get_revisions(pool: &PgPool, item_id: Uuid) -> Result<Vec<ItemRevision>> {
        // Every column the struct declares. `change_summary` and `ai_generated`
        // were added to `ItemRevision` and to the table (migrations
        // 20260330000004/5) and these two lists were not: `FromRow` wants a
        // column per field, so both of them failed at runtime for every caller,
        // which is why the revision list page answered 500 to everybody.
        let revisions = sqlx::query_as::<_, ItemRevision>(
            "SELECT id, item_id, author_id, title, status, fields, created, log, \
             change_summary, ai_generated, moderation_state FROM item_revision WHERE item_id = $1 \
             ORDER BY created DESC, id DESC",
        )
        .bind(item_id)
        .fetch_all(pool)
        .await
        .context("failed to fetch revisions")?;

        Ok(revisions)
    }

    /// Delete revision `revision_id` of item `item_id`, unless it is the item's
    /// current revision. `false` when no row matched: another item's revision,
    /// a revision already gone, or the current one.
    pub async fn delete_revision(pool: &PgPool, item_id: Uuid, revision_id: Uuid) -> Result<bool> {
        let result = sqlx::query(
            "DELETE FROM item_revision WHERE id = $1 AND item_id = $2 \
             AND id IS DISTINCT FROM (SELECT current_revision_id FROM item WHERE id = $2)",
        )
        .bind(revision_id)
        .bind(item_id)
        .execute(pool)
        .await
        .context("failed to delete revision")?;
        Ok(result.rows_affected() == 1)
    }

    /// The item's latest revision: the newest by time, then by id. On a
    /// moderated item it can be newer than the current revision (a draft
    /// ahead of the published one).
    pub async fn latest_revision(pool: &PgPool, item_id: Uuid) -> Result<Option<ItemRevision>> {
        sqlx::query_as::<_, ItemRevision>(
            "SELECT id, item_id, author_id, title, status, fields, created, log, \
             change_summary, ai_generated, moderation_state FROM item_revision \
             WHERE item_id = $1 ORDER BY created DESC, id DESC LIMIT 1",
        )
        .bind(item_id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch the latest revision")
    }

    /// Record a revision of item `id` that the item row does not take: a
    /// draft ahead of the published revision visitors keep seeing. Returns the
    /// revision's id.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_forward_revision(
        pool: &PgPool,
        id: Uuid,
        author_id: Uuid,
        title: &str,
        fields: &serde_json::Value,
        log: Option<&str>,
        moderation_state: &str,
    ) -> Result<Uuid> {
        let revision_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, log, moderation_state) \
             VALUES ($1, $2, $3, $4, 0, $5, $6, $7, $8)",
        )
        .bind(revision_id)
        .bind(id)
        .bind(author_id)
        .bind(title)
        .bind(fields)
        .bind(chrono::Utc::now().timestamp())
        .bind(log)
        .bind(moderation_state)
        .execute(pool)
        .await
        .context("failed to insert a forward revision")?;
        Ok(revision_id)
    }

    /// The items of moderated types whose latest revision is not published,
    /// newest change first: the moderated content list. `state` narrows it to
    /// one state. A revision saved before its type was moderated reads its
    /// state from its status, as `content::moderation::state_of` does.
    pub async fn list_moderated(
        pool: &PgPool,
        state: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ModeratedItem>> {
        sqlx::query_as::<_, ModeratedItem>(
            "SELECT id, item_type, title, author_id, changed, state, ahead FROM ( \
                 SELECT i.id, i.type AS item_type, r.title, r.author_id, r.created AS changed, \
                        COALESCE(r.moderation_state, \
                                 CASE WHEN r.status = 1 THEN 'published' ELSE 'draft' END) AS state, \
                        (r.id IS DISTINCT FROM i.current_revision_id) AS ahead \
                 FROM item i \
                 JOIN item_type t ON t.type = i.type AND t.settings -> 'moderation' = 'true'::jsonb \
                 JOIN LATERAL ( \
                     SELECT id, title, author_id, created, status, moderation_state \
                     FROM item_revision WHERE item_id = i.id \
                     ORDER BY created DESC, id DESC LIMIT 1 \
                 ) r ON true \
             ) latest \
             WHERE state <> 'published' AND ($1::text IS NULL OR state = $1) \
             ORDER BY changed DESC, id DESC LIMIT $2",
        )
        .bind(state)
        .bind(limit)
        .fetch_all(pool)
        .await
        .context("failed to list moderated items")
    }

    /// Get a specific revision.
    pub async fn get_revision(pool: &PgPool, revision_id: Uuid) -> Result<Option<ItemRevision>> {
        let revision = sqlx::query_as::<_, ItemRevision>(
            "SELECT id, item_id, author_id, title, status, fields, created, log, \
             change_summary, ai_generated, moderation_state FROM item_revision WHERE id = $1",
        )
        .bind(revision_id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch revision")?;

        Ok(revision)
    }

    /// The revisions of `item_id` among `revision_ids`, newest first.
    ///
    /// Scoped to the item in the query itself, so an id belonging to another
    /// item is simply not returned: a caller compares the result with what it
    /// asked for and treats a missing id as not found.
    pub async fn get_revisions_by_ids(
        pool: &PgPool,
        item_id: Uuid,
        revision_ids: &[Uuid],
    ) -> Result<Vec<ItemRevision>> {
        let revisions = sqlx::query_as::<_, ItemRevision>(
            "SELECT id, item_id, author_id, title, status, fields, created, log, \
             change_summary, ai_generated, moderation_state FROM item_revision \
             WHERE item_id = $1 AND id = ANY($2) ORDER BY created DESC, id DESC",
        )
        .bind(item_id)
        .bind(revision_ids)
        .fetch_all(pool)
        .await
        .context("failed to fetch revisions by id")?;

        Ok(revisions)
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
            author_id: None,
            created: None,
            stage_id: None,
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
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language, item_group_id, retention_days FROM item ORDER BY changed DESC LIMIT $1 OFFSET $2"
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .context("failed to list all items")?;

        Ok(items)
    }

    /// List items with optional filters, on every stage.
    ///
    /// Newest change first, ties broken by id so a page is stable.
    pub async fn list_filtered(
        pool: &PgPool,
        item_type: Option<&str>,
        status: Option<i16>,
        author_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>> {
        Self::list_where(pool, item_type, status, author_id, None, limit, offset).await
    }

    /// List items with optional filters, on `stage_ids` only: what a read made
    /// for a request may list (`item-api` `query-items`).
    pub async fn list_filtered_on_stages(
        pool: &PgPool,
        item_type: Option<&str>,
        status: Option<i16>,
        stage_ids: &[Uuid],
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>> {
        Self::list_where(
            pool,
            item_type,
            status,
            None,
            Some(stage_ids),
            limit,
            offset,
        )
        .await
    }

    async fn list_where(
        pool: &PgPool,
        item_type: Option<&str>,
        status: Option<i16>,
        author_id: Option<Uuid>,
        stage_ids: Option<&[Uuid]>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>> {
        // Build dynamic query
        let mut query = String::from(
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields, stage_id, language, item_group_id, retention_days FROM item WHERE 1=1",
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
        if stage_ids.is_some() {
            conditions.push(format!(" AND stage_id = ANY(${param_idx})"));
            param_idx += 1;
        }

        for cond in conditions {
            query.push_str(&cond);
        }

        query.push_str(&format!(
            " ORDER BY changed DESC, id DESC LIMIT ${} OFFSET ${}",
            param_idx,
            param_idx + 1
        ));

        let mut query_builder = sqlx::query_as::<_, Item>(AssertSqlSafe(query));

        if let Some(t) = item_type {
            query_builder = query_builder.bind(t);
        }
        if let Some(s) = status {
            query_builder = query_builder.bind(s);
        }
        if let Some(a) = author_id {
            query_builder = query_builder.bind(a);
        }
        if let Some(stages) = stage_ids {
            query_builder = query_builder.bind(stages.to_vec());
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

        let mut query_builder = sqlx::query_scalar::<_, i64>(AssertSqlSafe(query));

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

    /// One page of the admin content listing, and how many items match.
    ///
    /// The filters are optional: the type, the status, and `title_like`, an
    /// `ILIKE` pattern (see `routes::admin_list::ListQuery::like`).
    /// `order_by` must come from the listing's own allowed columns, never from
    /// the request; ties fall back to the newest change, then the id.
    pub async fn admin_page(
        pool: &PgPool,
        item_type: Option<&str>,
        status: Option<i16>,
        title_like: Option<&str>,
        order_by: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Self>, i64)> {
        let mut conditions = String::new();
        let mut n = 0;
        if item_type.is_some() {
            n += 1;
            conditions.push_str(&format!(" AND type = ${n}"));
        }
        if status.is_some() {
            n += 1;
            conditions.push_str(&format!(" AND status = ${n}"));
        }
        if title_like.is_some() {
            n += 1;
            conditions.push_str(&format!(" AND title ILIKE ${n}"));
        }
        // Ties fall back to the newest first, then the id, so a page is
        // stable; a sort by `changed` needs only the id after it.
        let ties = if order_by.starts_with("changed ") {
            "id DESC"
        } else {
            "changed DESC, id DESC"
        };
        let list_sql = format!(
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, \
             promote, sticky, fields, stage_id, language, item_group_id, retention_days \
             FROM item WHERE 1=1{conditions} ORDER BY {order_by}, {ties} \
             LIMIT ${} OFFSET ${}",
            n + 1,
            n + 2
        );
        let count_sql = format!("SELECT COUNT(*) FROM item WHERE 1=1{conditions}");

        let mut list = sqlx::query_as::<_, Item>(AssertSqlSafe(list_sql));
        let mut count = sqlx::query_scalar::<_, i64>(AssertSqlSafe(count_sql));
        if let Some(t) = item_type {
            list = list.bind(t);
            count = count.bind(t);
        }
        if let Some(s) = status {
            list = list.bind(s);
            count = count.bind(s);
        }
        if let Some(l) = title_like {
            list = list.bind(l);
            count = count.bind(l);
        }
        let items = list
            .bind(limit)
            .bind(offset)
            .fetch_all(pool)
            .await
            .context("failed to list the admin content page")?;
        let total = count
            .fetch_one(pool)
            .await
            .context("failed to count the admin content page")?;
        Ok((items, total))
    }

    /// Count all items.
    pub async fn count_all(pool: &PgPool) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item")
            .fetch_one(pool)
            .await
            .context("failed to count all items")?;

        Ok(count)
    }

    /// Find items that reference `target_id` in the `field_name` RecordReference
    /// field, via GIN-indexed JSONB containment (P11g / D-57).
    ///
    /// Replaces the former `LIMIT 50`-per-type reverse scan (`routes/item.rs`)
    /// that silently dropped referrers past the fiftieth. This is
    /// **superset-correct**: it returns *every* referencing item, matching whether
    /// the reference is stored as a bare UUID string (`"<uuid>"`, single-value
    /// RecordReference) or as an element of a JSON array (`["<uuid>", …]`,
    /// multi-value). Both `@>` containment forms are served by the existing
    /// `idx_item_fields` GIN index on `item(fields)`.
    ///
    /// `item_type` optionally scopes to one content type (the item-view page
    /// resolves references per declaring type). `field_name` is a validated
    /// content-type field name and `target_id` is bound as a JSONB **parameter**,
    /// so there is no interpolation surface.
    pub async fn find_referencing(
        pool: &PgPool,
        item_type: Option<&str>,
        field_name: &str,
        target_id: Uuid,
    ) -> Result<Vec<Self>> {
        let id_str = target_id.to_string();
        // The two storage shapes a RecordReference value can take.
        let scalar = serde_json::json!({ field_name: id_str });
        let array = serde_json::json!({ field_name: [id_str] });

        let mut sql = String::from(
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, \
             promote, sticky, fields, stage_id, language, item_group_id, retention_days \
             FROM item WHERE (fields @> $1::jsonb OR fields @> $2::jsonb)",
        );
        if item_type.is_some() {
            sql.push_str(" AND type = $3");
        }
        sql.push_str(" ORDER BY changed DESC");

        let mut query = sqlx::query_as::<_, Item>(AssertSqlSafe(sql))
            .bind(scalar)
            .bind(array);
        if let Some(t) = item_type {
            query = query.bind(t);
        }

        query
            .fetch_all(pool)
            .await
            .context("failed to find referencing items")
    }

    /// Count published items (live stage only).
    pub async fn count_published(pool: &PgPool) -> Result<i64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE status = 1 AND stage_id = $1")
                .bind(LIVE_STAGE_ID)
                .fetch_one(pool)
                .await
                .context("failed to count published items")?;

        Ok(count)
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
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
            stage_id: LIVE_STAGE_ID,
            language: "en".to_string(),
            item_group_id: Uuid::now_v7(),
            retention_days: None,
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
