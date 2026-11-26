//! The mail workers: claim queued messages, send them, record the outcome.
//!
//! BG18 (`docs/design/mail-queue.md`, "Workers"). A worker claims due rows of
//! `mail_outbox` with `FOR UPDATE SKIP LOCKED`, so two workers never hold the
//! same message; mints any one time secret; sends through the site's
//! [`MailTransport`]; and marks the row `sent`, or reschedules it with backoff
//! and jitter, or marks it `failed` (a permanent refusal, or the last attempt)
//! or `suppressed` (the address bounced or complained). Every message ends in
//! one of those or `cancelled`, with the reason on the row.
//!
//! Delivery is at least once. A worker that dies after the server accepted a
//! message and before it marked the row sends it again once the claim's lease
//! expires; the stable `Message-ID` lets the far end drop the copy.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use sqlx::{FromRow, PgPool};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::config::MailConfig;
use crate::services::email::{Attachment, EmailService, MailSecret, SECRET_MARKER};
use crate::services::mail_transport::{MailTransport, OutgoingMail, TransportError};

/// First retry delay, before jitter; doubles each attempt.
const BACKOFF_BASE_SECS: i64 = 30;
/// Longest retry delay.
const BACKOFF_CAP_SECS: i64 = 3600;
/// How long a limit count is trusted before it is counted again.
const LIMIT_REFRESH: Duration = Duration::from_secs(5);
/// How long a blog over a limit is left out of claims before it is checked
/// again.
const HOLD_FOR: Duration = Duration::from_secs(60);
/// How long a blog's `From` name and `Reply-To` are cached.
const SENDER_TTL: Duration = Duration::from_secs(60);

/// Which messages a worker takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Transactional mail only, one at a time, so account mail never waits
    /// behind a newsletter batch.
    Transactional,
    /// Anything, transactional first, in batches.
    Any,
}

/// Which rows a worker may claim. Production workers take [`DrainScope::All`];
/// the narrower scopes let a test drain its own messages and nobody else's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainScope {
    /// Every row.
    All,
    /// One blog's rows.
    Tenant(Uuid),
    /// Rows to these addresses (compared lower-cased).
    Recipients(Vec<String>),
}

/// What one pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DrainStats {
    /// Accepted by the transport.
    pub sent: u64,
    /// Failed for now, rescheduled.
    pub retried: u64,
    /// Failed for good.
    pub failed: u64,
    /// Not sent: the address is suppressed.
    pub suppressed: u64,
    /// Put back because the blog or the network is over a limit.
    pub held: u64,
    /// Put back unsent because the worker was asked to stop.
    pub released: u64,
}

impl DrainStats {
    fn add(&mut self, other: Self) {
        self.sent += other.sent;
        self.retried += other.retried;
        self.failed += other.failed;
        self.suppressed += other.suppressed;
        self.held += other.held;
        self.released += other.released;
    }

    /// Rows this pass claimed.
    pub fn claimed(&self) -> u64 {
        self.sent + self.retried + self.failed + self.suppressed + self.held + self.released
    }
}

/// A claimed row.
#[derive(Debug, FromRow)]
struct Claimed {
    id: i64,
    tenant_id: Uuid,
    kind: String,
    recipient: String,
    subject: String,
    text_body: Option<String>,
    html_body: Option<String>,
    attachments: Option<serde_json::Value>,
    unsubscribe_url: Option<String>,
    secret: Option<serde_json::Value>,
    attempts: i32,
    max_attempts: i32,
    priority: i16,
    suppressed: Option<String>,
}

/// A blog's `From` name and `Reply-To`.
#[derive(Debug, Clone)]
struct Sender {
    from_name: Option<String>,
    reply_to: Option<String>,
    /// The blog's `mail_limits` overrides, if any.
    limits: Option<serde_json::Value>,
}

/// Limit bookkeeping for one worker.
#[derive(Default)]
struct Limits {
    /// Counts of what was sent: `(tenant, kind)` → (when counted, last hour,
    /// last day).
    counts: HashMap<(Uuid, String), (Instant, i64, i64)>,
    /// Bulk sent network wide in the last hour, and when counted.
    network: Option<(Instant, i64)>,
}

/// One mail worker.
pub struct MailWorker {
    pool: PgPool,
    transport: Arc<dyn MailTransport>,
    config: MailConfig,
    from_email: String,
    name: String,
    lane: Lane,
    scope: DrainScope,
    limits: Mutex<Limits>,
    senders: Mutex<HashMap<Uuid, (Instant, Sender)>>,
}

impl MailWorker {
    /// A worker for `service`'s outbox.
    pub fn new(service: &EmailService, lane: Lane, scope: DrainScope) -> Self {
        let host = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "host".to_string());
        Self {
            pool: service.pool().clone(),
            transport: Arc::clone(service.transport()),
            config: service.config().clone(),
            from_email: service.sender_address().to_string(),
            name: format!("{host}:{}:{}", std::process::id(), Uuid::now_v7().simple()),
            lane,
            scope,
            limits: Mutex::new(Limits::default()),
            senders: Mutex::new(HashMap::new()),
        }
    }

    /// Run until `token` is cancelled: claim and send, then wait for a wake or
    /// the poll interval. On cancellation the message in flight finishes and
    /// every other claimed message is put back unsent.
    pub async fn run(self, token: CancellationToken, wake: Arc<Notify>) {
        let poll = Duration::from_secs(self.config.poll_secs);
        info!(worker = %self.name, lane = ?self.lane, "mail worker started");
        while !token.is_cancelled() {
            let claimed = match self.run_once(Some(&token)).await {
                Ok(stats) => stats.claimed(),
                Err(e) => {
                    warn!(worker = %self.name, error = %e, "mail worker pass failed");
                    0
                }
            };
            if claimed == 0 {
                tokio::select! {
                    () = wake.notified() => {}
                    () = tokio::time::sleep(poll) => {}
                    () = token.cancelled() => {}
                }
            }
        }
        info!(worker = %self.name, "mail worker stopped");
    }

    /// Claim and process until nothing is due.
    ///
    /// # Errors
    ///
    /// When the database fails.
    pub async fn drain(&self) -> Result<DrainStats> {
        let mut total = DrainStats::default();
        loop {
            let stats = self.run_once(None).await?;
            total.add(stats);
            if stats.claimed() == 0 || stats.claimed() == stats.held {
                return Ok(total);
            }
        }
    }

    /// Claim one batch and process it. With `stop`, a cancellation between
    /// messages puts the rest back unsent; so does reaching half the claim's
    /// lease, so that a slow server never has this worker start a send on a
    /// row another worker may already have reclaimed.
    ///
    /// # Errors
    ///
    /// When the claim fails. A failure while recording one message's outcome
    /// is logged; its lease brings it back.
    pub async fn run_once(&self, stop: Option<&CancellationToken>) -> Result<DrainStats> {
        let mut stats = DrainStats::default();
        let claimed_at = Instant::now();
        let send_window = Duration::from_secs(self.config.claim_lease_secs.max(0) as u64 / 2);
        let batch = self.claim().await?;
        if batch.is_empty() {
            return Ok(stats);
        }
        let (batch, over) = self.apply_limits(batch).await;
        for (key, ids, reason) in over {
            stats.held += self.release(&ids, Some(&reason)).await;
            self.hold_rest(&key, &reason).await;
        }

        let mut rest = batch.into_iter();
        while let Some(row) = rest.next() {
            if stop.is_some_and(CancellationToken::is_cancelled)
                || claimed_at.elapsed() >= send_window
            {
                let mut ids = vec![row.id];
                ids.extend(rest.map(|r| r.id));
                stats.released += self.release(&ids, None).await;
                break;
            }
            let id = row.id;
            match self.process(row).await {
                Ok(outcome) => stats.add(outcome),
                Err(e) => {
                    warn!(worker = %self.name, id, error = %e, "failed to record a mail outcome");
                }
            }
        }
        Ok(stats)
    }

    /// Claim due rows for this lane and scope, skipping rows another worker
    /// holds. A blog over a limit has its waiting rows pushed past the hold
    /// ([`Self::hold_rest`]), so they are simply not due.
    async fn claim(&self) -> Result<Vec<Claimed>> {
        let network_full = self.network_bulk_full().await?;
        let bulk_allowed = self.lane == Lane::Any && !network_full;
        // Bulk is the only kind at bulk priority, so the lane's bound is a
        // plain condition on the index's leading column: a worker that may not
        // take bulk never walks a bulk backlog.
        let max_priority = if bulk_allowed {
            crate::services::email::PRIORITY_BULK
        } else {
            crate::services::email::PRIORITY_NOTIFICATION
        };
        // As a list rather than a range, so the index is read one priority at
        // a time and each read stops at that priority's first row not yet due.
        let priorities: Vec<i16> = (0..=max_priority).collect();
        let limit = match self.lane {
            Lane::Transactional => 1,
            Lane::Any => self.config.batch,
        };
        let (tenant, recipients) = match &self.scope {
            DrainScope::All => (None, None),
            DrainScope::Tenant(t) => (Some(*t), None),
            DrainScope::Recipients(r) => (
                None,
                Some(r.iter().map(|a| a.to_lowercase()).collect::<Vec<_>>()),
            ),
        };

        let mut rows: Vec<Claimed> = sqlx::query_as(
            r#"
            WITH due AS (
                SELECT id FROM mail_outbox
                -- A claimed row's next_attempt_at is never after its claim, so
                -- the due test can come first and be checked on the index.
                WHERE next_attempt_at <= now()
                  AND (status = 'queued' OR (status = 'sending' AND locked_until <= now()))
                  AND priority = ANY($2::smallint[])
                  AND ($1 OR kind = 'transactional')
                  AND ($3::uuid IS NULL OR tenant_id = $3)
                  AND ($4::text[] IS NULL OR lower(recipient) = ANY($4))
                ORDER BY priority, next_attempt_at, id
                LIMIT $5
                FOR UPDATE SKIP LOCKED
            )
            UPDATE mail_outbox m
            SET status = 'sending',
                attempts = m.attempts + 1,
                locked_until = now() + make_interval(secs => $6),
                locked_by = $7,
                held_reason = NULL
            FROM due
            WHERE m.id = due.id
            RETURNING m.id, m.tenant_id, m.kind, m.recipient, m.subject, m.text_body,
                      m.html_body, m.attachments, m.unsubscribe_url, m.secret,
                      m.attempts, m.max_attempts, m.priority,
                      (SELECT s.reason FROM mail_suppression s
                       WHERE s.address = lower(m.recipient)) AS suppressed
            "#,
        )
        .bind(bulk_allowed)
        .bind(&priorities)
        .bind(tenant)
        .bind(recipients)
        .bind(limit)
        .bind(self.config.claim_lease_secs as f64)
        .bind(&self.name)
        .fetch_all(&self.pool)
        .await
        .context("failed to claim mail")?;
        // RETURNING keeps no order; send in the order the claim chose.
        rows.sort_by_key(|r| (r.priority, r.id));
        Ok(rows)
    }

    /// Whether bulk mail is over the network's hourly limit.
    async fn network_bulk_full(&self) -> Result<bool> {
        let limit = self.config.network_hourly_limit;
        if limit == 0 || self.lane == Lane::Transactional {
            return Ok(false);
        }
        let cached = self.limits.lock().network;
        let count = match cached {
            Some((at, n)) if at.elapsed() < LIMIT_REFRESH => n,
            _ => {
                let n: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM mail_outbox
                     WHERE kind = 'bulk' AND sent_at > now() - interval '1 hour'",
                )
                .fetch_one(&self.pool)
                .await
                .context("failed to count network mail")?;
                self.limits.lock().network = Some((Instant::now(), n));
                n
            }
        };
        Ok(count >= limit)
    }

    /// Split a claimed batch into what may be sent now and, per blog and kind
    /// over a limit, the ids to put back with the reason.
    #[allow(clippy::type_complexity)]
    async fn apply_limits(
        &self,
        batch: Vec<Claimed>,
    ) -> (Vec<Claimed>, Vec<((Uuid, String), Vec<i64>, String)>) {
        let pairs: HashSet<(Uuid, String)> = batch
            .iter()
            .map(|r| (r.tenant_id, r.kind.clone()))
            .collect();
        let mut allowance: HashMap<(Uuid, String), (i64, String)> = HashMap::new();
        for pair in pairs {
            match self.allowance(&pair).await {
                Ok(Some(left)) => {
                    allowance.insert(pair, left);
                }
                Ok(None) => {}
                Err(e) => {
                    // A count that fails holds nothing: limits protect the
                    // network's reputation, and a database blip is no reason
                    // to stop account mail.
                    warn!(error = %e, "failed to count mail for a limit; sending");
                }
            }
        }
        if allowance.is_empty() {
            return (batch, Vec::new());
        }

        let mut keep = Vec::with_capacity(batch.len());
        let mut over: HashMap<(Uuid, String), (Vec<i64>, String)> = HashMap::new();
        for row in batch {
            let key = (row.tenant_id, row.kind.clone());
            match allowance.get_mut(&key) {
                Some((left, reason)) if *left <= 0 => {
                    over.entry(key)
                        .or_insert_with(|| (Vec::new(), reason.clone()))
                        .0
                        .push(row.id);
                }
                Some((left, _)) => {
                    *left -= 1;
                    // Count it in the cache too, so the next batch inside the
                    // refresh window does not get the same allowance again.
                    if let Some((_, hour, day)) = self.limits.lock().counts.get_mut(&key) {
                        *hour += 1;
                        *day += 1;
                    }
                    keep.push(row);
                }
                None => keep.push(row),
            }
        }
        (
            keep,
            over.into_iter()
                .map(|(key, (ids, reason))| (key, ids, reason))
                .collect(),
        )
    }

    /// Hold the rest of a blog's waiting mail of `kind` for [`HOLD_FOR`]: push
    /// it past the hold with the reason, so no worker in any process claims it
    /// until the window may have room again. Nothing is dropped.
    async fn hold_rest(&self, (tenant, kind): &(Uuid, String), reason: &str) {
        let result = sqlx::query(
            "UPDATE mail_outbox
             SET next_attempt_at = now() + make_interval(secs => $4), held_reason = $3
             WHERE tenant_id = $1 AND kind = $2 AND status = 'queued'
               AND next_attempt_at <= now()",
        )
        .bind(tenant)
        .bind(kind)
        .bind(reason)
        .bind(HOLD_FOR.as_secs_f64())
        .execute(&self.pool)
        .await;
        if let Err(e) = result {
            warn!(worker = %self.name, error = %e, "failed to hold a blog's waiting mail");
        }
    }

    /// How many more messages of `kind` the blog may send now, and the reason
    /// to give when that is none; `None` when no limit applies.
    async fn allowance(&self, (tenant, kind): &(Uuid, String)) -> Result<Option<(i64, String)>> {
        let overrides = self.sender(*tenant).await.limits;
        let read = |key: &str, default: i64| {
            overrides
                .as_ref()
                .and_then(|o| o.get(key))
                .and_then(serde_json::Value::as_i64)
                .map_or(default, |v| v.max(0))
        };
        let (hourly, daily) = if kind == "bulk" {
            (
                read("hourly", self.config.blog_hourly_limit),
                read("daily", self.config.blog_daily_limit),
            )
        } else {
            (
                read(
                    "transactional_hourly",
                    self.config.blog_transactional_hourly_limit,
                ),
                0,
            )
        };
        if hourly == 0 && daily == 0 {
            return Ok(None);
        }

        let key = (*tenant, kind.clone());
        let cached = self.limits.lock().counts.get(&key).copied();
        let (hour, day) = match cached {
            Some((at, h, d)) if at.elapsed() < LIMIT_REFRESH => (h, d),
            _ => {
                let (h, d): (i64, i64) = sqlx::query_as(
                    "SELECT count(*) FILTER (WHERE sent_at > now() - interval '1 hour'),
                            count(*)
                     FROM mail_outbox
                     WHERE tenant_id = $1 AND kind = $2
                       AND sent_at > now() - make_interval(hours => $3)",
                )
                .bind(tenant)
                .bind(kind)
                // Only as far back as a limit looks: an hour unless a daily
                // limit applies.
                .bind(if daily > 0 { 24_i32 } else { 1_i32 })
                .fetch_one(&self.pool)
                .await
                .context("failed to count a blog's mail")?;
                self.limits
                    .lock()
                    .counts
                    .insert(key, (Instant::now(), h, d));
                (h, d)
            }
        };

        let mut left = i64::MAX;
        let mut reason = String::new();
        if hourly > 0 {
            left = hourly - hour;
            reason = format!("held: this blog's hourly {kind} mail limit ({hourly}) is reached");
        }
        if daily > 0 && daily - day < left {
            left = daily - day;
            reason = format!("held: this blog's daily {kind} mail limit ({daily}) is reached");
        }
        Ok(Some((left, reason)))
    }

    /// Put claimed rows back to `queued` without counting the attempt.
    async fn release(&self, ids: &[i64], held_reason: Option<&str>) -> u64 {
        let result = sqlx::query(
            "UPDATE mail_outbox
             SET status = 'queued', attempts = greatest(attempts - 1, 0),
                 locked_until = NULL, locked_by = NULL, held_reason = $3,
                 -- Held rows wait out the hold in the table too, so other
                 -- workers and processes do not claim and release them again.
                 next_attempt_at = CASE WHEN $3::text IS NULL THEN next_attempt_at
                                        ELSE now() + make_interval(secs => $4) END
             WHERE id = ANY($1) AND locked_by = $2 AND status = 'sending'",
        )
        .bind(ids)
        .bind(&self.name)
        .bind(held_reason)
        .bind(HOLD_FOR.as_secs_f64())
        .execute(&self.pool)
        .await;
        match result {
            Ok(r) => {
                if let Some(reason) = held_reason {
                    info!(worker = %self.name, count = r.rows_affected(), reason, "mail held");
                }
                r.rows_affected()
            }
            Err(e) => {
                warn!(worker = %self.name, error = %e, "failed to put claimed mail back; its lease will");
                0
            }
        }
    }

    /// Send one claimed row and record what happened.
    async fn process(&self, row: Claimed) -> Result<DrainStats> {
        let mut stats = DrainStats::default();

        if let Some(reason) = row.suppressed.as_deref()
            && (reason != "complaint" || row.kind == "bulk")
        {
            let why = match reason {
                "hard_bounce" => "suppressed: this address bounced",
                "complaint" => "suppressed: this address reported mail as spam",
                _ => "suppressed: an administrator stopped mail to this address",
            };
            self.finish(row.id, "suppressed", Some(why)).await?;
            stats.suppressed = 1;
            return Ok(stats);
        }

        // A message whose workers keep dying (the lease expired with the
        // attempt still counted) is not tried forever.
        if row.attempts > row.max_attempts {
            self.finish(
                row.id,
                "failed",
                Some("failed: the claim expired on every attempt; no worker finished sending it"),
            )
            .await?;
            stats.failed = 1;
            return Ok(stats);
        }

        let result = match self.outgoing(&row).await {
            Ok(mail) => self.transport.send(&mail).await,
            Err(e) => Err(e),
        };

        match result {
            Ok(()) => {
                let done = sqlx::query(
                    "UPDATE mail_outbox
                     SET status = 'sent', sent_at = now(), finished_at = now(),
                         locked_until = NULL, last_error = NULL
                     WHERE id = $1 AND locked_by = $2 AND status = 'sending'",
                )
                .bind(row.id)
                .bind(&self.name)
                .execute(&self.pool)
                .await
                .context("failed to mark mail sent")?;
                if done.rows_affected() == 0 {
                    warn!(
                        id = row.id,
                        "mail sent after its claim passed to another worker"
                    );
                }
                debug!(id = row.id, "mail sent");
                stats.sent = 1;
                if row.kind == "bulk"
                    && let Some((_, sent)) = self.limits.lock().network.as_mut()
                {
                    *sent += 1;
                }
            }
            Err(e) if e.permanent || row.attempts >= row.max_attempts => {
                let reason = if e.permanent {
                    format!("failed: {}", e.message)
                } else {
                    format!("failed after {} attempts: {}", row.attempts, e.message)
                };
                self.finish(row.id, "failed", Some(&reason)).await?;
                warn!(id = row.id, attempts = row.attempts, "mail failed for good");
                stats.failed = 1;
            }
            Err(e) => {
                let delay = backoff_secs(row.attempts);
                sqlx::query(
                    "UPDATE mail_outbox
                     SET status = 'queued', next_attempt_at = now() + make_interval(secs => $3),
                         last_error = $4, locked_until = NULL, locked_by = NULL
                     WHERE id = $1 AND locked_by = $2 AND status = 'sending'",
                )
                .bind(row.id)
                .bind(&self.name)
                .bind(delay as f64)
                .bind(format!(
                    "attempt {} of {}: {}",
                    row.attempts, row.max_attempts, e.message
                ))
                .execute(&self.pool)
                .await
                .context("failed to reschedule mail")?;
                info!(
                    id = row.id,
                    attempts = row.attempts,
                    delay_secs = delay,
                    "mail send failed; retrying later"
                );
                stats.retried = 1;
            }
        }
        Ok(stats)
    }

    /// Mark a row finished with `status`.
    async fn finish(&self, id: i64, status: &str, reason: Option<&str>) -> Result<()> {
        sqlx::query(
            "UPDATE mail_outbox
             SET status = $3, finished_at = now(), last_error = $4, locked_until = NULL
             WHERE id = $1 AND locked_by = $2 AND status = 'sending'",
        )
        .bind(id)
        .bind(&self.name)
        .bind(status)
        .bind(reason)
        .execute(&self.pool)
        .await
        .with_context(|| format!("failed to mark mail {status}"))?;
        Ok(())
    }

    /// Build the message: the blog's sender, the minted secret, the decoded
    /// attachments.
    async fn outgoing(&self, row: &Claimed) -> Result<OutgoingMail, TransportError> {
        let sender = self.sender(row.tenant_id).await;
        let Some(mut text) = row.text_body.clone() else {
            return Err(TransportError::permanent(
                "the body was cleared before it was sent",
            ));
        };
        let mut html = row.html_body.clone();
        if let Some(secret) = &row.secret {
            let secret: MailSecret = serde_json::from_value(secret.clone()).map_err(|e| {
                TransportError::permanent(format!("unreadable secret reference: {e}"))
            })?;
            let minted = mint(&self.pool, &secret, &row.recipient).await?;
            text = text.replace(SECRET_MARKER, &minted);
            html = html.map(|h| h.replace(SECRET_MARKER, &minted));
        }
        let attachments = decode_attachments(row.attachments.as_ref())?;
        let domain = self
            .from_email
            .rsplit_once('@')
            .map_or("localhost", |(_, d)| d);
        Ok(OutgoingMail {
            message_id: format!("outbox-{}@{domain}", row.id),
            from_name: sender.from_name,
            from_address: self.from_email.clone(),
            reply_to: sender
                .reply_to
                .filter(|r| !r.eq_ignore_ascii_case(&row.recipient)),
            to: row.recipient.clone(),
            subject: row.subject.clone(),
            text,
            html,
            attachments,
            unsubscribe_url: row.unsubscribe_url.clone(),
        })
    }

    /// The blog's `From` name and `Reply-To`, cached for a minute.
    async fn sender(&self, tenant: Uuid) -> Sender {
        if let Some((at, sender)) = self.senders.lock().get(&tenant)
            && at.elapsed() < SENDER_TTL
        {
            return sender.clone();
        }
        let read = |key: &'static str| {
            let pool = self.pool.clone();
            async move {
                crate::models::SiteConfig::get_for_tenant(&pool, key, tenant)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|v| v.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty() && !crate::services::email::has_control(s))
            }
        };
        let sender = Sender {
            from_name: read("site_name").await,
            reply_to: read("site_mail")
                .await
                .filter(|a| a.parse::<lettre::Address>().is_ok()),
            // A failed read means no overrides: the network defaults apply.
            limits: sqlx::query_scalar::<_, Option<serde_json::Value>>(
                "SELECT data->'mail_limits' FROM tenant WHERE id = $1",
            )
            .bind(tenant)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .flatten(),
        };
        self.senders
            .lock()
            .insert(tenant, (Instant::now(), sender.clone()));
        sender
    }
}

/// Decode stored attachments.
fn decode_attachments(
    value: Option<&serde_json::Value>,
) -> Result<Vec<Attachment>, TransportError> {
    use base64::Engine as _;
    let Some(items) = value.and_then(serde_json::Value::as_array) else {
        return Ok(Vec::new());
    };
    items
        .iter()
        .map(|item| {
            let field = |k: &str| {
                item.get(k)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
            };
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(field("bytes_base64"))
                .map_err(|e| TransportError::permanent(format!("unreadable attachment: {e}")))?;
            Ok(Attachment {
                filename: field("filename").to_string(),
                content_type: field("content_type").to_string(),
                bytes,
            })
        })
        .collect()
}

/// Mint the secret a row refers to. A missing account is permanent, and so is
/// an account whose address changed after the message was queued: the secret
/// goes only to the address the account has now (for an email change, the
/// address now pending), never to one it had at queue time.
async fn mint(
    pool: &PgPool,
    secret: &MailSecret,
    recipient: &str,
) -> Result<String, TransportError> {
    let user_id = match secret {
        MailSecret::PasswordReset { user_id }
        | MailSecret::Verification { user_id, .. }
        | MailSecret::RecoveryCode { user_id, .. } => *user_id,
    };
    let pending = matches!(
        secret,
        MailSecret::Verification { purpose, .. }
            if purpose == crate::models::email_verification::PURPOSE_EMAIL_CHANGE
    );
    let address: Option<Option<String>> = sqlx::query_scalar(
        "SELECT CASE WHEN $2 THEN data->>'pending_email' ELSE mail END FROM users WHERE id = $1",
    )
    .bind(user_id)
    .bind(pending)
    .fetch_optional(pool)
    .await
    .map_err(|e| TransportError::temporary(format!("could not check the account: {e}")))?;
    let Some(address) = address else {
        return Err(TransportError::permanent("the account no longer exists"));
    };
    if !address.is_some_and(|a| a.trim().eq_ignore_ascii_case(recipient.trim())) {
        return Err(TransportError::permanent(
            "the account's address changed after this message was queued; no secret minted",
        ));
    }
    let minted = match secret {
        MailSecret::PasswordReset { user_id } => {
            crate::models::password_reset::PasswordResetToken::create(pool, *user_id)
                .await
                .map(|(_, token)| token)
        }
        MailSecret::Verification { user_id, purpose } => {
            crate::models::email_verification::EmailVerificationToken::create(
                pool, *user_id, purpose,
            )
            .await
            .map(|(_, token)| token)
        }
        MailSecret::RecoveryCode { user_id, flow_id } => {
            match crate::services::recovery_builtins::mint_email_code(pool, *user_id, flow_id).await
            {
                Ok(Some(code)) => Ok(code),
                Ok(None) => {
                    return Err(TransportError::permanent(
                        "this recovery attempt's code was already used; no new code minted",
                    ));
                }
                Err(e) => Err(e),
            }
        }
    };
    minted
        .map_err(|e| TransportError::temporary(format!("could not mint the one-time secret: {e}")))
}

/// Retry delay after `attempts` failed attempts: 30 seconds doubling to an hour,
/// with equal jitter (half fixed, half random) so a recovered server is not met
/// by every retry at once.
pub fn backoff_secs(attempts: i32) -> i64 {
    use rand::Rng;
    let full = crate::cron::exponential_backoff_secs(BACKOFF_BASE_SECS, BACKOFF_CAP_SECS, attempts);
    let half = full / 2;
    half + rand::thread_rng().gen_range(0..=full - half)
}

/// Start the site's mail workers: `config.workers` of them, the first on the
/// transactional lane when there is more than one.
pub fn spawn_workers(
    service: &Arc<EmailService>,
    token: &CancellationToken,
) -> Vec<tokio::task::JoinHandle<()>> {
    let count = service.config().workers.max(1);
    (0..count)
        .map(|i| {
            let lane = if i == 0 && count > 1 {
                Lane::Transactional
            } else {
                Lane::Any
            };
            let worker = MailWorker::new(service, lane, DrainScope::All);
            let token = token.clone();
            let wake = service.wake_handle();
            tokio::spawn(async move { worker.run(token, wake).await })
        })
        .collect()
}

/// Retention: clear the bodies of finished messages older than
/// `body_retention_days`, and delete finished messages older than
/// `log_retention_days`, at most `batch` rows of each per call.
///
/// # Errors
///
/// When the database fails.
pub async fn purge(pool: &PgPool, config: &MailConfig, batch: i64) -> Result<(u64, u64)> {
    let cleared = sqlx::query(
        "UPDATE mail_outbox
         SET text_body = NULL, html_body = NULL, attachments = NULL, body_purged_at = now()
         WHERE id IN (
             SELECT id FROM mail_outbox
             WHERE finished_at < now() - make_interval(days => $1::int)
               AND body_purged_at IS NULL
             LIMIT $2)",
    )
    .bind(config.body_retention_days as i32)
    .bind(batch)
    .execute(pool)
    .await
    .context("failed to clear old mail bodies")?
    .rows_affected();
    let deleted = sqlx::query(
        "DELETE FROM mail_outbox
         WHERE id IN (
             SELECT id FROM mail_outbox
             WHERE finished_at < now() - make_interval(days => $1::int)
             LIMIT $2)",
    )
    .bind(config.log_retention_days as i32)
    .bind(batch)
    .execute(pool)
    .await
    .context("failed to delete old mail")?
    .rows_affected();
    Ok((cleared, deleted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_with_jitter_and_caps_at_an_hour() {
        for attempts in 1..=20 {
            let full = crate::cron::exponential_backoff_secs(30, 3600, attempts);
            for _ in 0..50 {
                let d = backoff_secs(attempts);
                assert!(
                    d >= full / 2 && d <= full,
                    "attempt {attempts}: {d} vs {full}"
                );
            }
        }
        assert_eq!(crate::cron::exponential_backoff_secs(30, 3600, 1), 30);
        assert_eq!(crate::cron::exponential_backoff_secs(30, 3600, 2), 60);
        assert_eq!(crate::cron::exponential_backoff_secs(30, 3600, 20), 3600);
    }

    #[test]
    fn stored_attachments_decode() {
        let value = serde_json::json!([
            {"filename": "a.txt", "content_type": "text/plain", "bytes_base64": "aGk="}
        ]);
        let decoded = decode_attachments(Some(&value)).unwrap_or_default();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].bytes, b"hi");
        assert!(decode_attachments(None).unwrap_or_default().is_empty());
    }
}
