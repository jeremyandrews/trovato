//! Items and their revisions, with the ids and dates the file gives.
//!
//! The statements here write what `Item::create_with_file_refs` and
//! `Item::update_with_file_refs` write, on the import's transaction instead
//! of one of their own, with the record's ids and dates instead of new ones.
//! The field checks are the ones `ItemService` runs on every create and
//! update, and the JSON API's (`docs/design/import.md`, "Validation").

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

use super::record::{ItemRecord, RevisionRecord};
use super::{
    Outcome, Run, require_flag, require_id, require_language, require_order, require_user,
};
use crate::content::file_refs::{self, FileReferences};
use crate::models::Item;
use crate::models::stage::LIVE_STAGE_ID;

const ITEM_COLUMNS: &str = "id, current_revision_id, type, title, author_id, status, created, \
     changed, promote, sticky, fields, stage_id, language, item_group_id, retention_days";

/// The stored item `id`, locked for the rest of the import.
async fn stored_item(run: &mut Run<'_>, id: Uuid) -> Result<Option<Item>> {
    let sql = format!("SELECT {ITEM_COLUMNS} FROM item WHERE id = $1 FOR UPDATE");
    sqlx::query_as::<_, Item>(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .fetch_optional(&mut *run.tx)
        .await
        .context("failed to look up the item")
}

/// Whether a revision `id` is stored, of any item.
async fn revision_exists(run: &mut Run<'_>, id: Uuid) -> Result<bool> {
    let found: Option<i32> = sqlx::query_scalar("SELECT 1 FROM item_revision WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *run.tx)
        .await
        .context("failed to look up a revision")?;
    Ok(found.is_some())
}

/// Check a title as the item forms do.
fn check_title(title: &str) -> Result<()> {
    if title.trim().is_empty() {
        anyhow::bail!("title is required");
    }
    if title.len() > crate::content::MAX_TITLE_LENGTH {
        anyhow::bail!(
            "title exceeds maximum length of {} characters",
            crate::content::MAX_TITLE_LENGTH
        );
    }
    Ok(())
}

/// Check `fields` against `item_type` as the forms, the JSON API and
/// `ItemService` check a write by an administrator, and return the file
/// references they hold. `previous` is the stored fields on a change.
async fn check_fields(
    run: &Run<'_>,
    item_type: &str,
    fields: &Value,
    previous: Option<&Value>,
) -> Result<FileReferences> {
    let Some(content_type) = run.state.content_types().get(item_type) else {
        anyhow::bail!("content type '{item_type}' does not exist");
    };
    let Some(sent) = fields.as_object() else {
        anyhow::bail!("fields must be a JSON object");
    };
    let mut refused = crate::content::field_value::check_json_fields(
        &content_type.fields,
        sent,
        previous.and_then(Value::as_object),
        &run.formats,
    );
    refused.extend(crate::content::compound::validate_required_fields(
        sent,
        &content_type.fields,
    ));
    if !refused.is_empty() {
        anyhow::bail!("{}", refused.join(" "));
    }
    run.state
        .items()
        .check_imported_fields(item_type, fields, previous)
        .await
}

/// Import one `item` record: create it, change it with a new revision, or
/// find it as the record says and leave it.
pub(crate) async fn import_item(run: &mut Run<'_>, r: ItemRecord) -> Result<Outcome> {
    require_id(r.id, "id")?;
    check_title(&r.title)?;
    require_order(r.created, r.changed)?;
    let status = r.status.unwrap_or(1);
    require_flag(status, "status")?;
    let (default_promote, default_sticky) =
        run.state.items().publishing_defaults(&r.item_type).await;
    let promote = r.promote.unwrap_or(default_promote);
    let sticky = r.sticky.unwrap_or(default_sticky);
    require_flag(promote, "promote")?;
    require_flag(sticky, "sticky")?;
    require_user(run, r.author_id, "author_id").await?;
    let revision_author = r.revision_author_id.unwrap_or(r.author_id);
    if revision_author != r.author_id {
        require_user(run, revision_author, "revision_author_id").await?;
    }
    let language = r
        .language
        .clone()
        .unwrap_or_else(|| run.default_language.clone());
    require_language(run, &language).await?;
    let fields = r
        .fields
        .clone()
        .unwrap_or_else(|| Value::Object(Default::default()));

    let stored = stored_item(run, r.id).await?;
    if let Some(stored) = &stored
        && stored.item_type != r.item_type
    {
        anyhow::bail!(
            "item {} is stored as a '{}', not a '{}'",
            r.id,
            stored.item_type,
            r.item_type
        );
    }
    let refs = check_fields(
        run,
        &r.item_type,
        &fields,
        stored.as_ref().map(|s| &s.fields),
    )
    .await?;

    if let Some(stored) = &stored
        && stored.title == r.title
        && stored.author_id == r.author_id
        && stored.status == status
        && stored.promote == promote
        && stored.sticky == sticky
        && stored.created == r.created
        && stored.changed == r.changed
        && stored.language == language
        && stored.fields == fields
    {
        return Ok(Outcome::Unchanged);
    }

    // The revision this write adds. A stored one is never rewritten
    // (revisions are immutable), so a given id must be a new one.
    let revision_id = match r.revision_id {
        Some(id) => {
            require_id(id, "revision_id")?;
            if revision_exists(run, id).await? {
                anyhow::bail!(
                    "revision {id} is already stored; a changed item needs a new \
                     revision_id, or none"
                );
            }
            id
        }
        None => Uuid::now_v7(),
    };

    if stored.is_none() {
        // As `Item::create_with_file_refs`: the item without a current
        // revision, its revision, then the item pointing at it. No stage
        // named: the default one, or Live if none is marked.
        sqlx::query(
            "INSERT INTO item (id, current_revision_id, type, title, author_id, status, created, \
             changed, promote, sticky, fields, stage_id, language, item_group_id) \
             VALUES ($1, NULL, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
             COALESCE((SELECT tag_id FROM stage_config WHERE is_default), $11), $12, $1)",
        )
        .bind(r.id)
        .bind(&r.item_type)
        .bind(&r.title)
        .bind(r.author_id)
        .bind(status)
        .bind(r.created)
        .bind(r.changed)
        .bind(promote)
        .bind(sticky)
        .bind(&fields)
        .bind(LIVE_STAGE_ID)
        .bind(&language)
        .execute(&mut *run.tx)
        .await
        .context("failed to insert the item")?;
    }

    sqlx::query(
        "INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, log) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(revision_id)
    .bind(r.id)
    .bind(revision_author)
    .bind(&r.title)
    .bind(status)
    .bind(&fields)
    .bind(r.changed)
    .bind(
        r.log
            .as_deref()
            .or(stored.is_none().then_some("Initial revision")),
    )
    .execute(&mut *run.tx)
    .await
    .context("failed to insert the revision")?;

    sqlx::query(
        "UPDATE item SET current_revision_id = $2, title = $3, author_id = $4, status = $5, \
         promote = $6, sticky = $7, created = $8, changed = $9, language = $10, fields = $11 \
         WHERE id = $1",
    )
    .bind(r.id)
    .bind(revision_id)
    .bind(&r.title)
    .bind(r.author_id)
    .bind(status)
    .bind(promote)
    .bind(sticky)
    .bind(r.created)
    .bind(r.changed)
    .bind(&language)
    .bind(&fields)
    .execute(&mut *run.tx)
    .await
    .context("failed to update the item")?;

    file_refs::sync_item_file_references(&mut run.tx, r.id, &refs).await?;

    Ok(if stored.is_none() {
        Outcome::Created
    } else {
        Outcome::Updated
    })
}

/// A stored revision, as a record would describe it.
#[derive(Debug, PartialEq, sqlx::FromRow)]
struct StoredRevision {
    item_id: Uuid,
    author_id: Uuid,
    title: String,
    status: i16,
    fields: Value,
    created: i64,
    log: Option<String>,
}

/// Import one `revision` record: a past revision of an item, written once
/// and never rewritten.
pub(crate) async fn import_revision(run: &mut Run<'_>, r: RevisionRecord) -> Result<Outcome> {
    require_id(r.id, "id")?;
    check_title(&r.title)?;
    let status = r.status.unwrap_or(1);
    require_flag(status, "status")?;
    let fields = r
        .fields
        .clone()
        .unwrap_or_else(|| Value::Object(Default::default()));

    let wanted = StoredRevision {
        item_id: r.item_id,
        author_id: r.author_id,
        title: r.title.clone(),
        status,
        fields: fields.clone(),
        created: r.created,
        log: r.log.clone(),
    };
    let stored: Option<StoredRevision> = sqlx::query_as(
        "SELECT item_id, author_id, title, status, fields, created, log \
         FROM item_revision WHERE id = $1",
    )
    .bind(r.id)
    .fetch_optional(&mut *run.tx)
    .await
    .context("failed to look up the revision")?;
    if let Some(stored) = stored {
        if stored == wanted {
            return Ok(Outcome::Unchanged);
        }
        anyhow::bail!(
            "revision {} is stored with other content, and revisions are never \
             rewritten; give the changed revision a new id",
            r.id
        );
    }

    let Some(item) = stored_item(run, r.item_id).await? else {
        anyhow::bail!(
            "item {} does not exist (stored, or on an earlier line)",
            r.item_id
        );
    };
    let current: Option<i64> =
        sqlx::query_scalar("SELECT created FROM item_revision WHERE id = $1")
            .bind(item.current_revision_id)
            .fetch_optional(&mut *run.tx)
            .await
            .context("failed to look up the item's current revision")?;
    if let Some(current) = current
        && r.created > current
    {
        anyhow::bail!(
            "a past revision cannot be newer ({}) than the item's current revision ({current})",
            r.created
        );
    }
    require_user(run, r.author_id, "author_id").await?;
    // A past revision's file references are not the item's: only the
    // current revision's are recorded, as a form save records them.
    check_fields(run, &item.item_type, &fields, None).await?;

    sqlx::query(
        "INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, log) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(r.id)
    .bind(r.item_id)
    .bind(r.author_id)
    .bind(&r.title)
    .bind(status)
    .bind(&fields)
    .bind(r.created)
    .bind(&r.log)
    .execute(&mut *run.tx)
    .await
    .context("failed to insert the revision")?;
    Ok(Outcome::Created)
}
