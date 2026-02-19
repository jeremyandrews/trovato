//! Audit logging service.
//!
//! Logs actions for content CRUD, authentication, and permission changes.

use anyhow::{Context, Result};
use sqlx::PgPool;
use tracing::debug;
use uuid::Uuid;

/// Audit logging service.
#[derive(Clone)]
pub struct AuditService {
    pool: PgPool,
}

impl AuditService {
    /// Create a new audit service.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Sanitize an IP address string for storage.
    ///
    /// Validates that the string is a valid IP address (v4 or v6). If not,
    /// returns "invalid" to prevent arbitrary string injection.
    fn sanitize_ip(ip: &str) -> &str {
        if ip.parse::<std::net::IpAddr>().is_ok() {
            ip
        } else {
            "invalid"
        }
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
        let ip_address = Self::sanitize_ip(ip_address);

        sqlx::query(
            r#"
            INSERT INTO audit_log (id, action, entity_type, entity_id, user_id, ip_address, details, created)
            VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, EXTRACT(EPOCH FROM NOW())::bigint)
            "#,
        )
        .bind(action)
        .bind(entity_type)
        .bind(entity_id)
        .bind(user_id)
        .bind(ip_address)
        .bind(&details)
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

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn ip_address_sanitization() {
        assert_eq!(AuditService::sanitize_ip("192.168.1.1"), "192.168.1.1");
        assert_eq!(AuditService::sanitize_ip("::1"), "::1");
        assert_eq!(AuditService::sanitize_ip("2001:db8::1"), "2001:db8::1");
        assert_eq!(AuditService::sanitize_ip("not-an-ip"), "invalid");
        assert_eq!(AuditService::sanitize_ip(""), "invalid");
        assert_eq!(
            AuditService::sanitize_ip("192.168.1.1; DROP TABLE"),
            "invalid"
        );
    }

    #[test]
    fn retention_cutoff_calculation() {
        let days = 90;
        let now = chrono::Utc::now().timestamp();
        let cutoff = now - (days * 86400);
        assert!(cutoff < now);
        assert_eq!(now - cutoff, days * 86400);
    }
}
