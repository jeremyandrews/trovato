//! Tenant model for multi-tenancy.
//!
//! Tenants isolate content, configuration, and users across multiple
//! sites on a single Trovato instance. Single-tenant installations use
//! the `DEFAULT_TENANT_ID` constant, which is seeded during installation.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Default tenant UUID — deterministic constant matching the seeded row.
///
/// Follows the `LIVE_STAGE_ID` pattern: a well-known UUID that single-tenant
/// sites use for all content. Multi-tenant sites create additional tenants.
pub const DEFAULT_TENANT_ID: Uuid = Uuid::from_bytes([
    0x01, 0x93, 0xa5, 0xa0, // time_high + version
    0x00, 0x01, // time_mid
    0x70, 0x00, // time_low + variant
    0x80, 0x00, // clock_seq
    0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // mac/host
]);

/// A tenant record.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Tenant {
    /// Unique identifier.
    pub id: Uuid,

    /// Human-readable name.
    pub name: String,

    /// Machine name (URL-safe, unique).
    pub machine_name: String,

    /// Whether this tenant is active.
    pub status: bool,

    /// Unix timestamp when created.
    pub created: i64,

    /// Arbitrary tenant metadata.
    pub data: serde_json::Value,
}

impl Tenant {
    /// Find a tenant by ID.
    pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Self>> {
        sqlx::query_as::<_, Tenant>("SELECT * FROM tenant WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .context("failed to fetch tenant")
    }

    /// Find a tenant by machine name.
    pub async fn find_by_machine_name(pool: &PgPool, machine_name: &str) -> Result<Option<Self>> {
        sqlx::query_as::<_, Tenant>("SELECT * FROM tenant WHERE machine_name = $1")
            .bind(machine_name)
            .fetch_optional(pool)
            .await
            .context("failed to fetch tenant by machine name")
    }
}

/// Tenant context resolved per request by the tenant middleware.
///
/// Stored in request extensions for downstream access.
#[derive(Debug, Clone)]
pub struct TenantContext {
    /// Tenant UUID.
    pub id: Uuid,

    /// Human-readable name.
    pub name: String,

    /// Machine name.
    pub machine_name: String,
}

impl TenantContext {
    /// Create the default tenant context for single-tenant installations.
    pub fn default_tenant() -> Self {
        Self {
            id: DEFAULT_TENANT_ID,
            name: "Default".to_string(),
            machine_name: "default".to_string(),
        }
    }
}
