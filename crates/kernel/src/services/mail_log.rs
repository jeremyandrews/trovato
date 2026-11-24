//! Reading and correcting the mail outbox (BG18 part 2,
//! `docs/design/mail-queue.md` §4 and §9).
//!
//! The admin pages and the bounce webhook go through here: the outbox list and
//! its per status counts, one message, **Retry** and **Cancel**, the network
//! suppression list, and recording a provider's bounce or complaint.
//!
//! Every state change is one guarded `UPDATE` that names the statuses it may
//! leave from, so it cannot race a worker: a worker only finishes rows it holds
//! as `sending` (`locked_by`), and nothing here touches a `sending` row.

use anyhow::Result;
use serde::Serialize;
use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::services::email::SECRET_MARKER;

/// Every outbox status, in the order the pages list them.
pub const STATUSES: &[&str] = &[
    "queued",
    "sending",
    "sent",
    "failed",
    "suppressed",
    "cancelled",
];

/// The two kinds of mail.
pub const KINDS: &[&str] = &["transactional", "bulk"];

/// What the stored body shows where the worker mints a one time secret.
pub const SECRET_PLACEHOLDER: &str = "[one time secret, minted when sent]";

/// Longest bounce detail kept, in characters.
const MAX_DETAIL_CHARS: usize = 500;

/// A filter over the outbox. `None` and empty fields match everything.
#[derive(Debug, Clone, Default)]
pub struct MailFilter {
    /// One blog only. The blog owner view always sets it.
    pub tenant: Option<Uuid>,
    pub status: Option<String>,
    pub kind: Option<String>,
    pub category: Option<String>,
    /// Matched case insensitively, whole address.
    pub recipient: Option<String>,
}

impl MailFilter {
    /// Add this filter's conditions to a query that already has a `WHERE`.
    fn push(&self, qb: &mut QueryBuilder<Postgres>, with_status: bool) {
        if let Some(tenant) = self.tenant {
            qb.push(" AND m.tenant_id = ").push_bind(tenant);
        }
        if with_status && let Some(status) = &self.status {
            qb.push(" AND m.status = ").push_bind(status.clone());
        }
        if let Some(kind) = &self.kind {
            qb.push(" AND m.kind = ").push_bind(kind.clone());
        }
        if let Some(category) = &self.category {
            qb.push(" AND m.category = ").push_bind(category.clone());
        }
        if let Some(recipient) = &self.recipient {
            qb.push(" AND lower(m.recipient) = ")
                .push_bind(recipient.trim().to_lowercase());
        }
    }
}

/// One message as the list shows it. Times are UTC, formatted.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct MailRow {
    pub id: i64,
    pub tenant_id: Uuid,
    pub blog: String,
    pub kind: String,
    pub category: String,
    pub recipient: String,
    pub subject: String,
    pub status: String,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub held_reason: Option<String>,
    pub created_at: String,
    pub sent_at: Option<String>,
    pub bounced: bool,
    pub complained: bool,
}

/// The columns a [`MailRow`] reads, `m` being `mail_outbox` and `t` `tenant`.
const ROW_COLUMNS: &str = "m.id, m.tenant_id, t.name AS blog, m.kind, m.category, m.recipient, \
     m.subject, m.status, m.attempts, m.last_error, m.held_reason, \
     to_char(m.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') AS created_at, \
     to_char(m.sent_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') AS sent_at, \
     m.bounced_at IS NOT NULL AS bounced, m.complained_at IS NOT NULL AS complained";

/// A page of messages matching `filter`, newest first.
pub async fn list(
    pool: &PgPool,
    filter: &MailFilter,
    limit: i64,
    offset: i64,
) -> Result<Vec<MailRow>> {
    let mut qb = QueryBuilder::new("SELECT ");
    qb.push(ROW_COLUMNS)
        .push(" FROM mail_outbox m JOIN tenant t ON t.id = m.tenant_id WHERE true");
    filter.push(&mut qb, true);
    qb.push(" ORDER BY m.id DESC LIMIT ")
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(offset);
    Ok(qb.build_query_as().fetch_all(pool).await?)
}

/// How many messages match `filter` in each status, every other condition
/// applied, so the counts read as "of what you are looking at". Every status
/// is present, zero or not.
pub async fn status_counts(pool: &PgPool, filter: &MailFilter) -> Result<Vec<(String, i64)>> {
    let mut qb = QueryBuilder::new("SELECT m.status, count(*) FROM mail_outbox m WHERE true");
    filter.push(&mut qb, false);
    qb.push(" GROUP BY m.status");
    let found: Vec<(String, i64)> = qb.build_query_as().fetch_all(pool).await?;
    Ok(STATUSES
        .iter()
        .map(|s| {
            let n = found.iter().find(|(f, _)| f == s).map_or(0, |(_, n)| *n);
            ((*s).to_string(), n)
        })
        .collect())
}

/// The blogs, for the blog filter: id and name, by name.
pub async fn blogs(pool: &PgPool) -> Result<Vec<(Uuid, String)>> {
    Ok(
        sqlx::query_as("SELECT id, name FROM tenant ORDER BY name, id")
            .fetch_all(pool)
            .await?,
    )
}

/// One message in full, as the detail page shows it.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct MailDetail {
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub row: MailRow,
    pub priority: i16,
    pub max_attempts: i32,
    pub dedupe_key: Option<String>,
    /// The stored text body, the secret marker replaced; `None` once
    /// retention cleared it.
    pub text_body: Option<String>,
    pub html_body: Option<String>,
    pub has_secret: bool,
    pub has_unsubscribe: bool,
    pub attachments: Option<serde_json::Value>,
    pub next_attempt_at: String,
    pub locked_by: Option<String>,
    pub locked_until: Option<String>,
    pub finished_at: Option<String>,
    pub bounced_at: Option<String>,
    pub complained_at: Option<String>,
    pub body_purged_at: Option<String>,
}

/// One message, or `None`. With `tenant`, only that blog's.
pub async fn detail(pool: &PgPool, id: i64, tenant: Option<Uuid>) -> Result<Option<MailDetail>> {
    let mut qb = QueryBuilder::new("SELECT ");
    qb.push(ROW_COLUMNS).push(
        ", m.priority, m.max_attempts, m.dedupe_key, m.text_body, m.html_body, \
         m.secret IS NOT NULL AS has_secret, m.unsubscribe_url IS NOT NULL AS has_unsubscribe, \
         (SELECT jsonb_agg(jsonb_build_object('filename', a->>'filename', \
                 'content_type', a->>'content_type')) \
          FROM jsonb_array_elements(m.attachments) a) AS attachments, \
         to_char(m.next_attempt_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') AS next_attempt_at, \
         m.locked_by, \
         to_char(m.locked_until AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') AS locked_until, \
         to_char(m.finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') AS finished_at, \
         to_char(m.bounced_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') AS bounced_at, \
         to_char(m.complained_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') AS complained_at, \
         to_char(m.body_purged_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') AS body_purged_at \
         FROM mail_outbox m JOIN tenant t ON t.id = m.tenant_id WHERE m.id = ",
    );
    qb.push_bind(id);
    if let Some(tenant) = tenant {
        qb.push(" AND m.tenant_id = ").push_bind(tenant);
    }
    let found: Option<MailDetail> = qb.build_query_as().fetch_optional(pool).await?;
    Ok(found.map(|mut d| {
        d.text_body = d.text_body.map(|b| redact_secret_marker(&b));
        d.html_body = d.html_body.map(|b| redact_secret_marker(&b));
        d
    }))
}

/// `body` with the one time secret marker shown as what it stands for.
pub fn redact_secret_marker(body: &str) -> String {
    body.replace(SECRET_MARKER, SECRET_PLACEHOLDER)
}

/// Put a failed or cancelled message back in the queue, its attempts reset.
/// `false` when it is in another status, or its body has been cleared (it
/// could only fail again).
pub async fn retry(pool: &PgPool, id: i64) -> Result<bool> {
    let done = sqlx::query(
        "UPDATE mail_outbox SET status = 'queued', attempts = 0, next_attempt_at = now(), \
             last_error = NULL, held_reason = NULL, finished_at = NULL, \
             locked_until = NULL, locked_by = NULL \
         WHERE id = $1 AND status IN ('failed', 'cancelled') AND text_body IS NOT NULL",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Cancel a queued message. `false` when it is not queued (a worker holds
/// it, or it has finished).
pub async fn cancel(pool: &PgPool, id: i64) -> Result<bool> {
    let done = sqlx::query(
        "UPDATE mail_outbox SET status = 'cancelled', finished_at = now(), \
             last_error = 'cancelled by an administrator', held_reason = NULL \
         WHERE id = $1 AND status = 'queued'",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// One suppressed address.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Suppression {
    pub address: String,
    pub reason: String,
    pub outbox_id: Option<i64>,
    pub detail: Option<String>,
    pub created_at: String,
}

/// A page of the suppression list, newest first, and the total. With
/// `address`, that address only.
pub async fn suppressions(
    pool: &PgPool,
    address: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<(Vec<Suppression>, i64)> {
    let address = address.map(|a| a.trim().to_lowercase());
    let rows = sqlx::query_as(
        "SELECT address, reason, outbox_id, detail, \
             to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') AS created_at \
         FROM mail_suppression WHERE ($1::text IS NULL OR address = $1) \
         ORDER BY created_at DESC, address LIMIT $2 OFFSET $3",
    )
    .bind(&address)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mail_suppression WHERE ($1::text IS NULL OR address = $1)",
    )
    .bind(&address)
    .fetch_one(pool)
    .await?;
    Ok((rows, total))
}

/// Stop all mail to `address`, by hand. An existing entry is kept as it is.
/// `false` when the address was already suppressed.
pub async fn suppress(pool: &PgPool, address: &str, detail: &str) -> Result<bool> {
    let done = sqlx::query(
        "INSERT INTO mail_suppression (address, reason, detail) VALUES ($1, 'manual', $2) \
         ON CONFLICT (address) DO NOTHING",
    )
    .bind(address.trim().to_lowercase())
    .bind(detail)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Remove `address` from the suppression list. `false` when it was not on it.
pub async fn unsuppress(pool: &PgPool, address: &str) -> Result<bool> {
    let done = sqlx::query("DELETE FROM mail_suppression WHERE address = $1")
        .bind(address.trim().to_lowercase())
        .execute(pool)
        .await?;
    Ok(done.rows_affected() == 1)
}

/// What a provider reported about a message it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feedback {
    /// The address does not exist or refuses mail for good: stop all mail.
    HardBounce,
    /// The recipient reported the message as spam: stop bulk mail.
    Complaint,
}

/// The outbox id in one of our `Message-ID`s (`outbox-{id}@domain`, angle
/// brackets optional), or `None` for any other message.
pub fn outbox_id_from_message_id(message_id: &str) -> Option<i64> {
    let id = message_id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>');
    let (local, _domain) = id.split_once('@')?;
    local
        .strip_prefix("outbox-")?
        .parse()
        .ok()
        .filter(|n| *n > 0)
}

/// Record a bounce or complaint for `address`, and on the outbox row `outbox_id`
/// names when it was sent to that address.
///
/// The suppression entry is network wide. A hard bounce replaces an existing
/// complaint (it stops more mail); anything else already there is kept. A
/// hard bounce marks the row `failed` with the provider's reason; a complaint
/// only stamps `complained_at`. Repeating an event changes nothing more.
pub async fn record_feedback(
    pool: &PgPool,
    feedback: Feedback,
    address: &str,
    outbox_id: Option<i64>,
    detail: &str,
) -> Result<()> {
    let address = address.trim().to_lowercase();
    let detail: String = detail.chars().take(MAX_DETAIL_CHARS).collect();
    let mut tx = pool.begin().await?;

    // Only a row that was sent to this address: a Message-ID from another
    // install sharing the sending domain names someone else's row.
    let outbox_id = match outbox_id {
        Some(id) => match feedback {
            Feedback::HardBounce => {
                sqlx::query_scalar::<_, i64>(
                    "UPDATE mail_outbox SET bounced_at = COALESCE(bounced_at, now()), \
                         status = 'failed', finished_at = COALESCE(finished_at, now()), \
                         last_error = $3 \
                     WHERE id = $1 AND lower(recipient) = $2 AND status IN ('sent', 'failed') \
                     RETURNING id",
                )
                .bind(id)
                .bind(&address)
                .bind(format!("hard bounce: {detail}"))
                .fetch_optional(&mut *tx)
                .await?
            }
            Feedback::Complaint => {
                sqlx::query_scalar::<_, i64>(
                    "UPDATE mail_outbox SET complained_at = COALESCE(complained_at, now()) \
                     WHERE id = $1 AND lower(recipient) = $2 AND status = 'sent' RETURNING id",
                )
                .bind(id)
                .bind(&address)
                .fetch_optional(&mut *tx)
                .await?
            }
        },
        None => None,
    };

    let reason = match feedback {
        Feedback::HardBounce => "hard_bounce",
        Feedback::Complaint => "complaint",
    };
    sqlx::query(
        "INSERT INTO mail_suppression (address, reason, outbox_id, detail) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (address) DO UPDATE SET reason = EXCLUDED.reason, \
             outbox_id = EXCLUDED.outbox_id, detail = EXCLUDED.detail, created_at = now() \
         WHERE mail_suppression.reason = 'complaint' AND EXCLUDED.reason = 'hard_bounce'",
    )
    .bind(&address)
    .bind(reason)
    .bind(outbox_id)
    .bind(Some(detail).filter(|d| !d.is_empty()))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_message_ids_name_their_row() {
        assert_eq!(outbox_id_from_message_id("outbox-7@example.test"), Some(7));
        assert_eq!(
            outbox_id_from_message_id("<outbox-42@mail.example>"),
            Some(42)
        );
        assert_eq!(outbox_id_from_message_id(" outbox-3@x "), Some(3));
    }

    #[test]
    fn other_message_ids_name_nothing() {
        for id in [
            "",
            "outbox-@x",
            "outbox-0@x",
            "outbox--1@x",
            "outbox-7",
            "other-7@x",
            "outbox-7x@x",
            "outbox-99999999999999999999@x",
        ] {
            assert_eq!(outbox_id_from_message_id(id), None, "{id}");
        }
    }

    #[test]
    fn the_secret_marker_is_shown_as_a_placeholder() {
        let body = format!("Reset: https://x.test/reset/{SECRET_MARKER}\n");
        let shown = redact_secret_marker(&body);
        assert!(!shown.contains(SECRET_MARKER));
        assert!(shown.contains(SECRET_PLACEHOLDER));
    }
}
