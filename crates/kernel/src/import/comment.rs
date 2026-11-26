//! Comments, with the ids and dates the file gives.
//!
//! The values are checked as the comment forms check them (a body, a subject
//! within its limit, a parent on the same item); the text format as for an
//! administrator: any the site has. Whether the item's type takes new
//! comments is not asked: that setting decides what visitors may post, and a
//! closed thread keeps the comments it already has. The database triggers set
//! the depth, the item's comment count and its search vector, as they do for
//! a comment posted through the site.

use anyhow::{Context, Result};
use uuid::Uuid;

use super::record::CommentRecord;
use super::{Outcome, Run, require_id, require_order, require_user};
use crate::models::comment::{CommentStatus, normalize_subject, subject_too_long};

/// A stored comment, as a record would describe it.
#[derive(Debug, PartialEq, sqlx::FromRow)]
struct StoredComment {
    item_id: Uuid,
    parent_id: Option<Uuid>,
    author_id: Uuid,
    subject: Option<String>,
    body: String,
    body_format: String,
    status: i16,
    created: i64,
    changed: i64,
}

/// Import one `comment` record.
pub(crate) async fn import_comment(run: &mut Run<'_>, r: CommentRecord) -> Result<Outcome> {
    require_id(r.id, "id")?;
    require_order(r.created, r.changed)?;
    if r.body.trim().is_empty() {
        anyhow::bail!("body is required");
    }
    let subject = normalize_subject(r.subject.as_deref());
    if subject.as_deref().is_some_and(subject_too_long) {
        anyhow::bail!(
            "subject is longer than {} characters",
            crate::models::comment::SUBJECT_MAX_CHARS
        );
    }
    let body_format = r
        .body_format
        .clone()
        .unwrap_or_else(|| "filtered_html".to_string());
    if !run.formats.contains(&body_format) {
        anyhow::bail!("text format '{body_format}' is not one the site has");
    }
    let status = r.status.unwrap_or(CommentStatus::Published.as_i16());
    if CommentStatus::from_i16(status).is_none() {
        anyhow::bail!("status must be 0, 1, 2 or 3, not {status}");
    }

    let wanted = StoredComment {
        item_id: r.item_id,
        parent_id: r.parent_id,
        author_id: r.author_id,
        subject,
        body: r.body.clone(),
        body_format,
        status,
        created: r.created,
        changed: r.changed,
    };
    let stored: Option<StoredComment> = sqlx::query_as(
        "SELECT item_id, parent_id, author_id, subject, body, body_format, status, created, \
         changed FROM comment WHERE id = $1 FOR UPDATE",
    )
    .bind(r.id)
    .fetch_optional(&mut *run.tx)
    .await
    .context("failed to look up the comment")?;
    if let Some(stored) = &stored {
        if *stored == wanted {
            return Ok(Outcome::Unchanged);
        }
        // The thread a comment sits in decides its depth, set once, when it
        // is written; moving it would leave the depth wrong.
        if stored.item_id != wanted.item_id || stored.parent_id != wanted.parent_id {
            anyhow::bail!(
                "comment {} is stored on another item or under another parent; \
                 a comment is not moved",
                r.id
            );
        }
    }

    let item: Option<i32> = sqlx::query_scalar("SELECT 1 FROM item WHERE id = $1")
        .bind(r.item_id)
        .fetch_optional(&mut *run.tx)
        .await
        .context("failed to look up the item")?;
    if item.is_none() {
        anyhow::bail!(
            "item {} does not exist (stored, or on an earlier line)",
            r.item_id
        );
    }
    if let Some(parent) = r.parent_id {
        let on: Option<Uuid> = sqlx::query_scalar("SELECT item_id FROM comment WHERE id = $1")
            .bind(parent)
            .fetch_optional(&mut *run.tx)
            .await
            .context("failed to look up the parent comment")?;
        match on {
            None => anyhow::bail!(
                "parent comment {parent} does not exist (stored, or on an earlier line)"
            ),
            Some(item) if item != r.item_id => {
                anyhow::bail!("parent comment {parent} is on another item")
            }
            Some(_) => {}
        }
    }
    require_user(run, r.author_id, "author_id").await?;

    if stored.is_some() {
        sqlx::query(
            "UPDATE comment SET author_id = $2, subject = $3, body = $4, body_format = $5, \
             status = $6, created = $7, changed = $8 WHERE id = $1",
        )
        .bind(r.id)
        .bind(wanted.author_id)
        .bind(&wanted.subject)
        .bind(&wanted.body)
        .bind(&wanted.body_format)
        .bind(wanted.status)
        .bind(wanted.created)
        .bind(wanted.changed)
        .execute(&mut *run.tx)
        .await
        .context("failed to update the comment")?;
        return Ok(Outcome::Updated);
    }
    sqlx::query(
        "INSERT INTO comment (id, item_id, parent_id, author_id, subject, body, body_format, \
         status, created, changed) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(r.id)
    .bind(wanted.item_id)
    .bind(wanted.parent_id)
    .bind(wanted.author_id)
    .bind(&wanted.subject)
    .bind(&wanted.body)
    .bind(&wanted.body_format)
    .bind(wanted.status)
    .bind(wanted.created)
    .bind(wanted.changed)
    .execute(&mut *run.tx)
    .await
    .context("failed to insert the comment")?;
    Ok(Outcome::Created)
}
