//! The kernel-internal append-only security audit stream.
//!
//! One stream, structured events, one bounded retention policy — the
//! audit-events direction Jeremy set 2026-07-14 (recorded in the decision log
//! 2026-07-17 alongside the Epic-4 ratification). Every security-relevant state
//! change in the kernel emits here: authentication outcomes, passkey lifecycle,
//! password lifecycle, session lifecycle (the D-36 rider), and account recovery
//! (including the D-32 fold-escalation amendment).
//!
//! # What this is not
//!
//! It is **not** [`crate::services::audit::AuditService`], which is the
//! content-CRUD audit belonging to the optional `trovato_audit_log` plugin and
//! is `None` when that plugin is disabled. This module is kernel
//! infrastructure: non-optional, always written, and the single durable record
//! incident response reads. The two are deliberately separate streams because
//! they answer different questions and have different availability guarantees.
//!
//! # 1.0 posture: kernel-internal only
//!
//! There is **no plugin-facing audit host interface** in 1.0. That is what keeps
//! this subsystem off the PF-5 frozen-contract path entirely — nothing here is
//! visible across the WASM boundary, so nothing here can be a breaking change to
//! the 1.0 plugin contract. A post-1.0 additive host interface remains possible
//! via an opt-in `host_interfaces` entry.
//!
//! # Never write credential-adjacent material
//!
//! Session identifiers, credential identifiers, and recovery flow nonces are
//! stored only as a stable SHA-256 hex digest via [`hash_subject`] — never in
//! the clear (the D-36 rider is explicit about session ids). Tokens, codes,
//! passwords, and password hashes never enter this stream at all. The
//! [`SecurityEvent`] builder has no field that takes one, which is the point:
//! the API makes the mistake awkward rather than merely forbidden.
//!
//! # Retention
//!
//! One bounded policy for the whole stream:
//! [`DEFAULT_RETENTION_DAYS`], overridable with `SECURITY_AUDIT_RETENTION_DAYS`.
//! The cron `cleanup_security_audit_log` task prunes by age. There is
//! deliberately no per-event-kind retention: one stream, one policy, so nobody
//! has to reason about which events survived.

use anyhow::{Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

/// Default retention for the security audit stream, in days.
///
/// A year: long enough that an incident discovered late still has a trail,
/// bounded so the table cannot grow without limit. Override with
/// `SECURITY_AUDIT_RETENTION_DAYS`.
pub const DEFAULT_RETENTION_DAYS: i64 = 365;

/// Maximum stored length of a `User-Agent`, in bytes.
///
/// User-Agent is attacker-controlled and unbounded; truncating at the write site
/// keeps a hostile client from using the audit stream as free storage.
const MAX_USER_AGENT_LEN: usize = 512;

/// The structured kinds of event this stream carries.
///
/// The dotted `<domain>.<event>` string form is what lands in the `kind` column
/// and what alerting greps for; it is stable and must not be renamed without a
/// migration, because stored rows carry the old spelling forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityEventKind {
    // ── Passkey / credential lifecycle (Stories 4.1, 4.2, 4.3) ──────────────
    /// A passkey was registered to an account.
    PasskeyRegistered,
    /// A passkey registration ceremony failed (bad attestation, expired
    /// challenge, duplicate credential).
    PasskeyRegistrationFailed,
    /// A passkey's user-assigned device name changed.
    PasskeyRenamed,
    /// A passkey was removed from an account.
    PasskeyRevoked,
    /// A credential or password removal was refused because it would have left
    /// the account with no way in (the ≥1-active-recovery-path invariant, D-33).
    CredentialRemovalBlocked,

    // ── Authentication (Story 4.2) ──────────────────────────────────────────
    /// An authentication succeeded, by the method named in `details.method`.
    LoginSucceeded,
    /// An authentication failed, by the method named in `details.method`.
    LoginFailed,
    /// An authenticator presented a signature counter at or below the stored
    /// value (D-37). The authentication is rejected and the credential flagged;
    /// it is deliberately **not** auto-revoked.
    PasskeyCounterRegression,

    // ── Password lifecycle (Story 4.3) ──────────────────────────────────────
    /// A password was set on an account that had none.
    PasswordSet,
    /// An existing password was changed.
    PasswordChanged,
    /// A password was removed — the account is now passwordless.
    PasswordRemoved,

    // ── Session lifecycle (Story 4.4, the D-36 rider) ───────────────────────
    /// A session was created (login, or the session established after recovery).
    SessionCreated,
    /// A user revoked one of their own sessions.
    SessionRevokedByUser,
    /// An admin revoked another account's session.
    SessionRevokedByAdmin,
    /// A session id was cycled (the fixation defence firing on an auth-state
    /// change). `subject_hash` is the hash of the **new** id.
    SessionIdCycled,

    // ── Account recovery (Story 4.6) ────────────────────────────────────────
    /// A recovery flow was initiated for an account.
    RecoveryInitiated,
    /// A recovery method was chosen and its challenge started.
    RecoveryMethodInitiated,
    /// A recovery `verify` was folded to an outcome.
    RecoveryVerdict,
    /// A recovery flow completed and granted the scoped credential-reset state
    /// (D-38 — never a standing session).
    RecoveryCompleted,
    /// A recovery attempt was refused by the rate limiter.
    RecoveryRateLimited,
    /// **Alert-worthy.** The D-32 fold ignored a `Verified` from a plugin that
    /// does not own the `method_id` namespace: a plugin attempted to escalate
    /// into an account it has no claim on.
    RecoveryFoldEscalationAttempt,
    /// The owning plugin answered a recovery op with the wrong result shape, or
    /// with output the kernel could not parse. Casts no vote (fail-closed).
    RecoveryFoldWrongShape,
}

impl SecurityEventKind {
    /// The stable dotted string stored in the `kind` column.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PasskeyRegistered => "passkey.registered",
            Self::PasskeyRegistrationFailed => "passkey.registration_failed",
            Self::PasskeyRenamed => "passkey.renamed",
            Self::PasskeyRevoked => "passkey.revoked",
            Self::CredentialRemovalBlocked => "credential.removal_blocked",
            Self::LoginSucceeded => "auth.login_succeeded",
            Self::LoginFailed => "auth.login_failed",
            Self::PasskeyCounterRegression => "passkey.counter_regression",
            Self::PasswordSet => "password.set",
            Self::PasswordChanged => "password.changed",
            Self::PasswordRemoved => "password.removed",
            Self::SessionCreated => "session.created",
            Self::SessionRevokedByUser => "session.revoked_by_user",
            Self::SessionRevokedByAdmin => "session.revoked_by_admin",
            Self::SessionIdCycled => "session.id_cycled",
            Self::RecoveryInitiated => "recovery.initiated",
            Self::RecoveryMethodInitiated => "recovery.method_initiated",
            Self::RecoveryVerdict => "recovery.verdict",
            Self::RecoveryCompleted => "recovery.completed",
            Self::RecoveryRateLimited => "recovery.rate_limited",
            Self::RecoveryFoldEscalationAttempt => "recovery.fold_escalation_attempt",
            Self::RecoveryFoldWrongShape => "recovery.fold_wrong_shape",
        }
    }
}

impl std::fmt::Display for SecurityEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether the audited operation succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The operation completed.
    Success,
    /// The operation was refused or errored. Failures are first-class rows: an
    /// auth audit trail that only records successes answers no useful question.
    Failure,
}

impl Outcome {
    /// The stable string stored in the `outcome` column.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }
}

/// A stable, non-reversible digest of an opaque identifier.
///
/// Used for session ids (the D-36 rider forbids storing them raw), credential
/// ids, and recovery flow nonces. SHA-256 hex: stable across processes and
/// restarts, so the same session correlates across its whole lifecycle, while
/// the stored value is useless as the identifier itself.
pub fn hash_subject(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    hex::encode(hasher.finalize())
}

/// One append-only security event, built field-by-field and handed to
/// [`SecurityAudit::record`].
///
/// There is no field for a token, code, password, or raw session id. Identifiers
/// go through [`SecurityEvent::subject`], which hashes them on the way in.
#[derive(Debug, Clone)]
pub struct SecurityEvent {
    kind: SecurityEventKind,
    outcome: Outcome,
    user_id: Option<Uuid>,
    actor_id: Option<Uuid>,
    subject_hash: Option<String>,
    ip_address: Option<String>,
    user_agent: Option<String>,
    details: serde_json::Value,
}

impl SecurityEvent {
    /// Start a successful event of the given kind.
    pub fn new(kind: SecurityEventKind) -> Self {
        Self {
            kind,
            outcome: Outcome::Success,
            user_id: None,
            actor_id: None,
            subject_hash: None,
            ip_address: None,
            user_agent: None,
            details: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    /// Start a failed event of the given kind.
    pub fn failure(kind: SecurityEventKind) -> Self {
        Self {
            outcome: Outcome::Failure,
            ..Self::new(kind)
        }
    }

    /// The account this event is about.
    #[must_use]
    pub fn user(mut self, user_id: Uuid) -> Self {
        self.user_id = Some(user_id);
        self
    }

    /// The account this event is about, when it may be unknown.
    #[must_use]
    pub fn maybe_user(mut self, user_id: Option<Uuid>) -> Self {
        self.user_id = user_id;
        self
    }

    /// The actor who caused the event, when different from the subject account
    /// (an admin acting on another user).
    #[must_use]
    pub fn actor(mut self, actor_id: Uuid) -> Self {
        self.actor_id = Some(actor_id);
        self
    }

    /// The opaque identifier this event is about (session id, credential id,
    /// recovery flow nonce). **Hashed here**; the raw value is never stored.
    #[must_use]
    pub fn subject(mut self, raw_identifier: &str) -> Self {
        self.subject_hash = Some(hash_subject(raw_identifier));
        self
    }

    /// The vetted client IP (`ClientIp`) for the request that caused the event.
    #[must_use]
    pub fn ip(mut self, ip: impl Into<String>) -> Self {
        self.ip_address = Some(ip.into());
        self
    }

    /// The request `User-Agent`, truncated to a bounded length.
    #[must_use]
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        let mut ua = ua.into();
        if ua.len() > MAX_USER_AGENT_LEN {
            // Truncate on a char boundary — a multi-byte UA must not panic here.
            let mut cut = MAX_USER_AGENT_LEN;
            while cut > 0 && !ua.is_char_boundary(cut) {
                cut -= 1;
            }
            ua.truncate(cut);
        }
        self.user_agent = Some(ua);
        self
    }

    /// Attach a structured detail field. Never pass secrets.
    #[must_use]
    pub fn detail(mut self, key: &str, value: impl Serialize) -> Self {
        let encoded = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
        if let Some(obj) = self.details.as_object_mut() {
            obj.insert(key.to_string(), encoded);
        }
        self
    }

    /// The event kind (for tests and for the tracing mirror).
    pub fn kind(&self) -> SecurityEventKind {
        self.kind
    }

    /// The recorded outcome (for tests).
    pub fn outcome(&self) -> Outcome {
        self.outcome
    }

    /// The hashed subject, if one was set (for tests).
    pub fn subject_hash(&self) -> Option<&str> {
        self.subject_hash.as_deref()
    }

    /// The accumulated structured detail (for tests).
    pub fn details(&self) -> &serde_json::Value {
        &self.details
    }
}

/// One row read back out of the stream.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SecurityAuditRow {
    /// Row identifier.
    pub id: Uuid,
    /// The dotted event kind (see [`SecurityEventKind::as_str`]).
    pub kind: String,
    /// The account the event is about, if known.
    pub user_id: Option<Uuid>,
    /// The acting account, when different from `user_id`.
    pub actor_id: Option<Uuid>,
    /// Hashed opaque identifier, if the event had one.
    pub subject_hash: Option<String>,
    /// `success` or `failure`.
    pub outcome: String,
    /// Validated client IP.
    pub ip_address: Option<String>,
    /// Truncated User-Agent.
    pub user_agent: Option<String>,
    /// Structured, secret-free detail.
    pub details: serde_json::Value,
    /// Unix seconds.
    pub created: i64,
}

/// The append-only security audit stream.
///
/// Writes are best-effort at the call site: an audit failure must never break
/// the security operation it is recording (refusing a login because the audit
/// table is unreachable would turn an observability outage into an availability
/// outage). Use [`SecurityAudit::record`] when the caller can handle an error,
/// and [`SecurityAudit::emit`] at the many call sites that just want the event
/// written and a loud log line if it could not be.
#[derive(Clone)]
pub struct SecurityAudit {
    pool: PgPool,
}

impl SecurityAudit {
    /// Create the stream over the kernel's pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Validate an IP string, mirroring the `AuditService::sanitize_ip`
    /// precedent: anything that is not a parseable address becomes `"invalid"`
    /// rather than being stored verbatim.
    fn sanitize_ip(ip: &str) -> String {
        if ip.parse::<std::net::IpAddr>().is_ok() {
            ip.to_string()
        } else {
            "invalid".to_string()
        }
    }

    /// Append one event, returning an error if the write failed.
    pub async fn record(&self, event: SecurityEvent) -> Result<()> {
        let ip = event.ip_address.as_deref().map(Self::sanitize_ip);

        sqlx::query(
            r#"
            INSERT INTO security_audit_log
                (kind, user_id, actor_id, subject_hash, outcome, ip_address, user_agent, details)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(event.kind.as_str())
        .bind(event.user_id)
        .bind(event.actor_id)
        .bind(&event.subject_hash)
        .bind(event.outcome.as_str())
        .bind(&ip)
        .bind(&event.user_agent)
        .bind(&event.details)
        .execute(&self.pool)
        .await
        .context("failed to append security audit event")?;

        Ok(())
    }

    /// Append one event, logging loudly on failure instead of propagating.
    ///
    /// This is the shape almost every call site wants: the security operation
    /// has already happened, and losing its audit row is serious enough to log
    /// at `error` but never serious enough to fail the request.
    pub async fn emit(&self, event: SecurityEvent) {
        let kind = event.kind;
        if let Err(e) = self.record(event).await {
            tracing::error!(
                error = %e,
                kind = %kind,
                "SECURITY AUDIT WRITE FAILED — event lost"
            );
        }
    }

    /// Read recent events for one account, newest first.
    pub async fn recent_for_user(
        &self,
        user_id: Uuid,
        limit: i64,
    ) -> Result<Vec<SecurityAuditRow>> {
        sqlx::query_as::<_, SecurityAuditRow>(
            r#"
            SELECT id, kind, user_id, actor_id, subject_hash, outcome,
                   ip_address, user_agent, details, created
            FROM security_audit_log
            WHERE user_id = $1
            ORDER BY created DESC, id DESC
            LIMIT $2
            "#,
        )
        .bind(user_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to read security audit events for user")
    }

    /// Read recent events of one kind, newest first. Used by tests and by
    /// alerting on the escalation-attempt kinds.
    pub async fn recent_of_kind(
        &self,
        kind: SecurityEventKind,
        limit: i64,
    ) -> Result<Vec<SecurityAuditRow>> {
        sqlx::query_as::<_, SecurityAuditRow>(
            r#"
            SELECT id, kind, user_id, actor_id, subject_hash, outcome,
                   ip_address, user_agent, details, created
            FROM security_audit_log
            WHERE kind = $1
            ORDER BY created DESC, id DESC
            LIMIT $2
            "#,
        )
        .bind(kind.as_str())
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to read security audit events by kind")
    }

    /// Prune events older than the retention window. One policy, whole stream.
    pub async fn prune(&self, retention_days: i64) -> Result<u64> {
        let cutoff = chrono::Utc::now().timestamp() - (retention_days * 86_400);

        let result = sqlx::query("DELETE FROM security_audit_log WHERE created < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .context("failed to prune security audit log")?;

        Ok(result.rows_affected())
    }
}

impl std::fmt::Debug for SecurityAudit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityAudit").finish()
    }
}

/// Read the configured retention window from `SECURITY_AUDIT_RETENTION_DAYS`.
///
/// A one-line edge over `retention_days_from`, which holds the resolution so
/// that it can be tested without mutating the process environment.
pub fn retention_days_from_env() -> i64 {
    retention_days_from(
        std::env::var("SECURITY_AUDIT_RETENTION_DAYS")
            .ok()
            .as_deref(),
    )
}

/// Resolve the retention window from a configured value, falling back to
/// [`DEFAULT_RETENTION_DAYS`] when it is absent, unparseable, or not positive.
///
/// Non-positive is rejected rather than honoured because `prune` deletes
/// everything older than `now - days`: a zero or negative window would empty the
/// whole stream on the next cron run, which is the opposite of a retention
/// policy.
pub(crate) fn retention_days_from(value: Option<&str>) -> i64 {
    value
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|d| *d > 0)
        .unwrap_or(DEFAULT_RETENTION_DAYS)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn kind_strings_are_stable_and_namespaced() {
        assert_eq!(
            SecurityEventKind::PasskeyRegistered.as_str(),
            "passkey.registered"
        );
        assert_eq!(
            SecurityEventKind::SessionRevokedByAdmin.as_str(),
            "session.revoked_by_admin"
        );
        assert_eq!(
            SecurityEventKind::RecoveryFoldEscalationAttempt.as_str(),
            "recovery.fold_escalation_attempt"
        );
    }

    #[test]
    fn every_kind_string_is_unique_and_dotted() {
        let kinds = [
            SecurityEventKind::PasskeyRegistered,
            SecurityEventKind::PasskeyRegistrationFailed,
            SecurityEventKind::PasskeyRenamed,
            SecurityEventKind::PasskeyRevoked,
            SecurityEventKind::CredentialRemovalBlocked,
            SecurityEventKind::LoginSucceeded,
            SecurityEventKind::LoginFailed,
            SecurityEventKind::PasskeyCounterRegression,
            SecurityEventKind::PasswordSet,
            SecurityEventKind::PasswordChanged,
            SecurityEventKind::PasswordRemoved,
            SecurityEventKind::SessionCreated,
            SecurityEventKind::SessionRevokedByUser,
            SecurityEventKind::SessionRevokedByAdmin,
            SecurityEventKind::SessionIdCycled,
            SecurityEventKind::RecoveryInitiated,
            SecurityEventKind::RecoveryMethodInitiated,
            SecurityEventKind::RecoveryVerdict,
            SecurityEventKind::RecoveryCompleted,
            SecurityEventKind::RecoveryRateLimited,
            SecurityEventKind::RecoveryFoldEscalationAttempt,
            SecurityEventKind::RecoveryFoldWrongShape,
        ];
        let mut seen = std::collections::HashSet::new();
        for k in kinds {
            let s = k.as_str();
            assert!(s.contains('.'), "{s} is not <domain>.<event>");
            assert!(seen.insert(s), "duplicate kind string {s}");
        }
    }

    #[test]
    fn subject_is_hashed_never_stored_raw() {
        let raw = "s3ss10n-1d-that-must-never-land-in-the-table";
        let event = SecurityEvent::new(SecurityEventKind::SessionCreated).subject(raw);
        let hash = event.subject_hash().unwrap();
        assert_ne!(hash, raw);
        assert_eq!(hash.len(), 64, "SHA-256 hex is 64 chars");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn subject_hash_is_stable_across_calls() {
        // Correlating a session across its whole lifecycle depends on this.
        assert_eq!(hash_subject("abc"), hash_subject("abc"));
        assert_ne!(hash_subject("abc"), hash_subject("abd"));
    }

    #[test]
    fn failure_events_are_marked_failure() {
        let e = SecurityEvent::failure(SecurityEventKind::LoginFailed);
        assert_eq!(e.outcome(), Outcome::Failure);
        assert_eq!(e.outcome().as_str(), "failure");
        assert_eq!(
            SecurityEvent::new(SecurityEventKind::LoginSucceeded)
                .outcome()
                .as_str(),
            "success"
        );
    }

    #[test]
    fn details_accumulate_as_a_json_object() {
        let e = SecurityEvent::new(SecurityEventKind::LoginSucceeded)
            .detail("method", "passkey")
            .detail("credential_count", 2);
        assert_eq!(e.details()["method"], "passkey");
        assert_eq!(e.details()["credential_count"], 2);
    }

    #[test]
    fn user_agent_truncates_without_splitting_a_char() {
        // A multi-byte UA longer than the cap must truncate on a boundary.
        let ua = "é".repeat(MAX_USER_AGENT_LEN);
        let e = SecurityEvent::new(SecurityEventKind::SessionCreated).user_agent(ua);
        let stored = e.user_agent.unwrap();
        assert!(stored.len() <= MAX_USER_AGENT_LEN);
        // Round-tripping proves we did not cut mid-codepoint.
        assert!(stored.chars().all(|c| c == 'é'));
    }

    #[test]
    fn ip_sanitization_rejects_injection() {
        assert_eq!(SecurityAudit::sanitize_ip("192.168.1.1"), "192.168.1.1");
        assert_eq!(SecurityAudit::sanitize_ip("::1"), "::1");
        assert_eq!(SecurityAudit::sanitize_ip("'; DROP TABLE --"), "invalid");
        assert_eq!(SecurityAudit::sanitize_ip(""), "invalid");
    }

    /// The window is always a positive number of days, whatever it is
    /// configured with.
    ///
    /// Driven through `retention_days_from` rather than the env edge, so
    /// "nothing configured" is a value this test passes in rather than a
    /// property of the shell that happens to be running it.
    #[test]
    fn retention_window_is_always_positive() {
        const { assert!(DEFAULT_RETENTION_DAYS > 0) };
        assert_eq!(retention_days_from(None), DEFAULT_RETENTION_DAYS);
        assert_eq!(retention_days_from(Some("30")), 30);
        // Non-positive and unparseable both fall back: a zero-day window would
        // prune the entire stream.
        assert_eq!(retention_days_from(Some("0")), DEFAULT_RETENTION_DAYS);
        assert_eq!(retention_days_from(Some("-5")), DEFAULT_RETENTION_DAYS);
        assert_eq!(retention_days_from(Some("")), DEFAULT_RETENTION_DAYS);
        assert_eq!(retention_days_from(Some("ninety")), DEFAULT_RETENTION_DAYS);
        assert_eq!(retention_days_from(Some("30 days")), DEFAULT_RETENTION_DAYS);
    }
}
