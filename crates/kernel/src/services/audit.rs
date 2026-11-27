//! Audit logging service, and the one helper every write service records
//! through.
//!
//! [`AuditService`] writes a row to the `trovato_audit_log` plugin's
//! `audit_log` table. [`AuditTrail`] is what the write services hold: it
//! records a content row through [`AuditService::log`] when that plugin is
//! enabled, and a security event through
//! [`SecurityAudit::emit`](crate::audit::SecurityAudit::emit) always. Who
//! acted, and from where, comes from the request the write is part of
//! ([`AuditActor`], scoped by `middleware::scope_audit_actor`), so no write
//! service needs a request in its signature to be audited. A write made
//! outside a request (cron, a queue job) has no request actor and records
//! the actor its caller passes, or none.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::RwLock;
use sqlx::PgPool;
use tracing::debug;
use uuid::Uuid;

use crate::audit::{SecurityAudit, SecurityEvent};

/// The plugin whose `audit_log` table content rows are written to.
pub const AUDIT_LOG_PLUGIN: &str = "trovato_audit_log";

/// Audit logging service.
#[derive(Clone)]
pub struct AuditService {
    pool: PgPool,
    ip_storage: crate::client_addr::ClientIpStorage,
}

impl AuditService {
    /// Create a new audit service, storing client addresses in full.
    pub fn new(pool: PgPool) -> Self {
        Self::with_ip_storage(pool, crate::client_addr::ClientIpStorage::Full)
    }

    /// Create a new audit service that stores of each client address only what
    /// `ip_storage` (`CLIENT_IP_STORAGE`) allows. The column is `NOT NULL`, so
    /// "nothing" is the empty string.
    pub fn with_ip_storage(pool: PgPool, ip_storage: crate::client_addr::ClientIpStorage) -> Self {
        Self { pool, ip_storage }
    }

    /// Log an auditable action.
    pub async fn log(
        &self,
        action: &str,
        entity_type: &str,
        entity_id: &str,
        user_id: Option<Uuid>,
        ip_address: &str,
        details: serde_json::Value,
    ) -> Result<()> {
        // Anything that is not an address is stored as `"invalid"`, never
        // verbatim. A write with no request behind it (cron, the command
        // line) has no address at all, which is stored as nothing.
        let ip_address = if ip_address.is_empty() {
            String::new()
        } else {
            self.ip_storage.apply(ip_address).unwrap_or_default()
        };

        // A v7 id, so rows written in the same second still read back in the
        // order they were written.
        sqlx::query(
            r#"
            INSERT INTO audit_log (id, action, entity_type, entity_id, user_id, ip_address, details, created)
            VALUES ($7, $1, $2, $3, $4, $5, $6, EXTRACT(EPOCH FROM NOW())::bigint)
            "#,
        )
        .bind(action)
        .bind(entity_type)
        .bind(entity_id)
        .bind(user_id)
        .bind(ip_address)
        .bind(&details)
        .bind(Uuid::now_v7())
        .execute(&self.pool)
        .await
        .context("failed to write audit log")?;

        debug!(
            action = %action,
            entity_type = %entity_type,
            entity_id = %entity_id,
            "audit log entry created"
        );

        Ok(())
    }

    /// Cleanup old audit log entries beyond retention period.
    pub async fn cleanup(&self, retention_days: i64) -> Result<u64> {
        let cutoff = chrono::Utc::now().timestamp() - (retention_days * 86400);

        let result = sqlx::query("DELETE FROM audit_log WHERE created < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await;

        match result {
            Ok(res) => Ok(res.rows_affected()),
            Err(e) => {
                if e.to_string().contains("audit_log") {
                    debug!("audit_log table not found, skipping cleanup");
                    Ok(0)
                } else {
                    Err(e).context("failed to cleanup audit log")
                }
            }
        }
    }
}

impl std::fmt::Debug for AuditService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditService").finish()
    }
}

/// Who is acting in the request currently being handled, and from where.
#[derive(Debug, Clone, Default)]
pub struct AuditActor {
    /// The signed-in user (session or API token), `None` for a visitor.
    pub user_id: Option<Uuid>,
    /// The vetted client address (`ClientIp`).
    pub ip: String,
    /// The request's `User-Agent`.
    pub user_agent: String,
}

tokio::task_local! {
    /// Set for the length of a request by `middleware::scope_audit_actor`.
    static AUDIT_ACTOR: AuditActor;
}

impl AuditActor {
    /// The actor of the request being handled, or an empty one outside a
    /// request (and in a task spawned from one, which loses the scope).
    pub fn current() -> Self {
        AUDIT_ACTOR.try_with(Clone::clone).unwrap_or_default()
    }

    /// Run `fut` with `self` as the request's actor.
    pub async fn scope<F: std::future::Future>(self, fut: F) -> F::Output {
        AUDIT_ACTOR.scope(self, fut).await
    }
}

/// The one audit helper the write services call.
///
/// Cheap to clone. [`AuditTrail::disabled`] records nothing, for a service
/// built outside `AppState` (a test constructing one directly).
#[derive(Clone, Default)]
pub struct AuditTrail {
    inner: Option<Arc<AuditTrailInner>>,
}

struct AuditTrailInner {
    content: AuditService,
    security: SecurityAudit,
    /// The running site's enabled plugins, shared with `AppState`, so enabling
    /// or disabling `trovato_audit_log` is seen at the next write.
    enabled_plugins: Arc<RwLock<HashSet<String>>>,
}

impl AuditTrail {
    /// The trail for a running site.
    pub fn new(
        pool: PgPool,
        ip_storage: crate::client_addr::ClientIpStorage,
        enabled_plugins: Arc<RwLock<HashSet<String>>>,
    ) -> Self {
        Self {
            inner: Some(Arc::new(AuditTrailInner {
                content: AuditService::with_ip_storage(pool.clone(), ip_storage),
                security: SecurityAudit::with_ip_storage(pool, ip_storage),
                enabled_plugins,
            })),
        }
    }

    /// The trail for a process with no running site (the command line):
    /// which plugins are enabled is read from the database once.
    pub async fn offline(pool: PgPool) -> Self {
        let enabled = crate::plugin::status::get_enabled_names(&pool)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
        Self::new(
            pool,
            crate::client_addr::ClientIpStorage::Full,
            Arc::new(RwLock::new(enabled)),
        )
    }

    /// A trail that records nothing.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Record one content audit row, when `trovato_audit_log` is enabled.
    ///
    /// `actor` is who acted when the caller knows (a service handed a
    /// `UserContext`); `None` takes the request's signed-in user. A failed
    /// write is logged and swallowed: the change it describes has happened.
    pub async fn record(
        &self,
        action: &str,
        entity_type: &str,
        entity_id: impl std::fmt::Display,
        actor: Option<Uuid>,
        details: serde_json::Value,
    ) {
        let Some(inner) = &self.inner else { return };
        if !inner.enabled_plugins.read().contains(AUDIT_LOG_PLUGIN) {
            return;
        }
        let request = AuditActor::current();
        let user_id = actor.filter(|id| !id.is_nil()).or(request.user_id);
        let entity_id = entity_id.to_string();
        if let Err(e) = inner
            .content
            .log(
                action,
                entity_type,
                &entity_id,
                user_id,
                &request.ip,
                details,
            )
            .await
        {
            tracing::error!(
                error = %e,
                action,
                entity_type,
                entity_id = %entity_id,
                "audit log write failed"
            );
        }
    }

    /// Emit one security event, filling in the request's actor, address and
    /// `User-Agent` where the event does not already name them.
    ///
    /// Best effort, on the pool: for an event with no stored change behind
    /// it. An event that records a change is written with
    /// [`Self::security_in`].
    pub async fn security(&self, event: SecurityEvent) {
        let Some(inner) = &self.inner else { return };
        inner
            .security
            .emit(event.with_request_context(&AuditActor::current()))
            .await;
    }

    /// Write one security event on `conn`, inside the transaction that makes
    /// the change it records, filling in the request's actor, address and
    /// `User-Agent` as [`Self::security`] does. The change and its event
    /// commit together or not at all.
    ///
    /// # Errors
    ///
    /// When the insert fails. The caller must then fail its transaction: a
    /// change whose event cannot be written does not happen. A trail that
    /// records nothing ([`Self::disabled`]) writes nothing and succeeds.
    pub async fn security_in(
        &self,
        conn: &mut sqlx::PgConnection,
        event: SecurityEvent,
    ) -> anyhow::Result<()> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };
        inner
            .security
            .record_in(conn, event.with_request_context(&AuditActor::current()))
            .await
    }

    /// [`Self::security_in`] without the request's context: the event is
    /// written exactly as the caller built it, for a caller that names every
    /// field its event carries (a route that writes the address it vetted
    /// and no `User-Agent`).
    ///
    /// # Errors
    ///
    /// As [`Self::security_in`].
    pub async fn security_as_built_in(
        &self,
        conn: &mut sqlx::PgConnection,
        event: SecurityEvent,
    ) -> anyhow::Result<()> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };
        inner.security.record_in(conn, event).await
    }

    /// [`Self::security_in`] for each of `events`, in order.
    ///
    /// # Errors
    ///
    /// As [`Self::security_in`], at the first event that fails.
    pub async fn security_all_in(
        &self,
        conn: &mut sqlx::PgConnection,
        events: impl IntoIterator<Item = SecurityEvent>,
    ) -> anyhow::Result<()> {
        for event in events {
            self.security_in(conn, event).await?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for AuditTrail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditTrail")
            .field("enabled", &self.inner.is_some())
            .finish()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    #[test]
    fn retention_cutoff_calculation() {
        let days = 90;
        let now = chrono::Utc::now().timestamp();
        let cutoff = now - (days * 86400);
        assert!(cutoff < now);
        assert_eq!(now - cutoff, days * 86400);
    }
}
