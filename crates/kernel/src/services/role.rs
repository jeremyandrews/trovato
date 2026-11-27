//! Role service with permission cache invalidation.
//!
//! Wraps role and permission CRUD operations, ensuring that the
//! [`PermissionService`] cache is
//! invalidated whenever role permissions or user-role assignments change.

use std::sync::Arc;

use anyhow::Result;
use sqlx::PgPool;
use tracing::info;
use uuid::Uuid;

use crate::audit::{SecurityEvent, SecurityEventKind};
use crate::models::Role;
use crate::permissions::PermissionService;
use crate::services::audit::AuditTrail;

/// Service for role CRUD and permission management.
///
/// Always present in [`AppState`](crate::state::AppState) as `Arc<RoleService>`.
/// Initialized after `PermissionService` (dependency order).
#[derive(Clone)]
pub struct RoleService {
    inner: Arc<RoleServiceInner>,
}

struct RoleServiceInner {
    pool: PgPool,
    permissions: PermissionService,
    audit: AuditTrail,
}

/// Write `user.roles_changed` for a role assigned to (`change = "added"`) or
/// removed from (`"removed"`) an account, on `conn`: the transaction that
/// makes the change, so the two commit together. Returns the role's name,
/// for [`record_membership`] once the change has committed.
///
/// [`RoleService`] calls it for every membership change it makes, and the
/// command line and `trovato import`, which write memberships without a
/// running site, call it directly.
///
/// # Errors
///
/// When the role cannot be read or the event cannot be written; the caller's
/// transaction must then roll back.
pub async fn membership_event_in(
    audit: &AuditTrail,
    conn: &mut sqlx::PgConnection,
    user_id: Uuid,
    role_id: Uuid,
    change: &str,
) -> Result<String> {
    let name = Role::find_by_id(&mut *conn, role_id)
        .await?
        .map(|r| r.name)
        .unwrap_or_default();
    audit
        .security_in(
            conn,
            SecurityEvent::new(SecurityEventKind::UserRolesChanged)
                .user(user_id)
                .detail("change", change)
                .detail("role", &name)
                .detail("role_id", role_id),
        )
        .await?;
    Ok(name)
}

/// Record a committed membership change (see [`membership_event_in`]) as a
/// content audit row, when `trovato_audit_log` is enabled.
pub async fn record_membership(
    audit: &AuditTrail,
    user_id: Uuid,
    role_id: Uuid,
    name: &str,
    change: &str,
) {
    audit
        .record(
            "roles",
            "user",
            user_id,
            None,
            serde_json::json!({ "change": change, "role": name, "role_id": role_id }),
        )
        .await;
}

/// The security event for a role written, deleted or renamed.
fn role_event(kind: SecurityEventKind, role_id: Uuid, name: &str) -> SecurityEvent {
    SecurityEvent::new(kind)
        .detail("role_id", role_id)
        .detail("role", name)
}

/// What changed in a role's grants, for its events and its audit row.
struct GrantChange {
    name: String,
    granted: Vec<String>,
    revoked: Vec<String>,
}

impl RoleService {
    /// Create a new role service.
    pub fn new(pool: PgPool, permissions: PermissionService, audit: AuditTrail) -> Self {
        Self {
            inner: Arc::new(RoleServiceInner {
                pool,
                permissions,
                audit,
            }),
        }
    }

    /// Record a committed role write as a content audit row.
    async fn record_role(&self, action: &str, role_id: Uuid, name: &str) {
        self.inner
            .audit
            .record(
                action,
                "role",
                role_id,
                None,
                serde_json::json!({ "name": name }),
            )
            .await;
    }

    /// Compare a role's grants now with `before`, on `tx`, and write
    /// `role.permissions_changed` there if they differ. Returns the change,
    /// for [`Self::record_grants`] once it has committed.
    async fn grants_event_in(
        &self,
        tx: &mut sqlx::PgConnection,
        role_id: Uuid,
        before: &[String],
    ) -> Result<Option<GrantChange>> {
        let after = Role::get_permissions(&mut *tx, role_id).await?;
        let granted: Vec<String> = after
            .iter()
            .filter(|p| !before.contains(p))
            .cloned()
            .collect();
        let revoked: Vec<String> = before
            .iter()
            .filter(|p| !after.contains(p))
            .cloned()
            .collect();
        if granted.is_empty() && revoked.is_empty() {
            return Ok(None);
        }
        let name = Role::find_by_id(&mut *tx, role_id)
            .await?
            .map(|r| r.name)
            .unwrap_or_default();
        self.inner
            .audit
            .security_in(
                tx,
                SecurityEvent::new(SecurityEventKind::RolePermissionsChanged)
                    .detail("role_id", role_id)
                    .detail("role", &name)
                    .detail("granted", &granted)
                    .detail("revoked", &revoked),
            )
            .await?;
        Ok(Some(GrantChange {
            name,
            granted,
            revoked,
        }))
    }

    /// Record a committed change to a role's grants as a content audit row.
    async fn record_grants(&self, role_id: Uuid, change: Option<GrantChange>) {
        let Some(change) = change else { return };
        self.inner
            .audit
            .record(
                "permissions",
                "role",
                role_id,
                None,
                serde_json::json!({
                    "name": change.name,
                    "granted": change.granted,
                    "revoked": change.revoked,
                }),
            )
            .await;
    }

    /// Change a role's grants with `apply`, on one transaction that also
    /// writes the event recording the change, then invalidate the cache and
    /// write the audit row.
    async fn change_grants<F>(&self, role_id: Uuid, apply: F) -> Result<()>
    where
        F: for<'c> FnOnce(
            &'c mut sqlx::PgConnection,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<()>> + Send + 'c>,
        >,
    {
        let mut tx = self.inner.pool.begin().await?;
        let before = Role::get_permissions(&mut *tx, role_id).await?;
        apply(&mut tx).await?;
        let change = self.grants_event_in(&mut tx, role_id, &before).await?;
        tx.commit().await?;
        self.inner.permissions.invalidate_all();
        self.record_grants(role_id, change).await;
        Ok(())
    }

    /// Find a role by ID.
    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<Role>> {
        Role::find_by_id(&self.inner.pool, id).await
    }

    /// Find a role by name.
    pub async fn find_by_name(&self, name: &str) -> Result<Option<Role>> {
        Role::find_by_name(&self.inner.pool, name).await
    }

    /// List all roles.
    pub async fn list(&self) -> Result<Vec<Role>> {
        Role::list(&self.inner.pool).await
    }

    /// Create a new role. The role and `role.created` commit together.
    pub async fn create(&self, name: &str) -> Result<Role> {
        let mut tx = self.inner.pool.begin().await?;
        let role = Role::create(&mut *tx, name).await?;
        self.inner
            .audit
            .security_in(
                &mut tx,
                role_event(SecurityEventKind::RoleCreated, role.id, &role.name),
            )
            .await?;
        tx.commit().await?;
        info!(role_id = %role.id, name = %role.name, "role created");
        self.record_role("create", role.id, &role.name).await;
        Ok(role)
    }

    /// Update a role's name. The rename and `role.changed` commit together.
    pub async fn update(&self, id: Uuid, name: &str) -> Result<Option<Role>> {
        let mut tx = self.inner.pool.begin().await?;
        let role = Role::update(&mut *tx, id, name).await?;
        if let Some(ref r) = role {
            self.inner
                .audit
                .security_in(
                    &mut tx,
                    role_event(SecurityEventKind::RoleChanged, r.id, &r.name),
                )
                .await?;
        }
        tx.commit().await?;
        if let Some(ref r) = role {
            // Cached access carries role names, so a rename changes it.
            self.inner.permissions.invalidate_all();
            info!(role_id = %r.id, name = %r.name, "role updated");
            self.record_role("update", r.id, &r.name).await;
        }
        Ok(role)
    }

    /// Delete a role.
    ///
    /// Prevents deletion of well-known roles (anonymous, authenticated).
    /// Invalidates the entire permission cache because any user with this
    /// role will have different effective permissions after deletion. The
    /// delete and `role.deleted` commit together.
    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        let mut tx = self.inner.pool.begin().await?;
        let name = Role::find_by_id(&mut *tx, id)
            .await?
            .map(|r| r.name)
            .unwrap_or_default();
        let deleted = Role::delete(&mut *tx, id).await?;
        if deleted {
            self.inner
                .audit
                .security_in(
                    &mut tx,
                    role_event(SecurityEventKind::RoleDeleted, id, &name),
                )
                .await?;
        }
        tx.commit().await?;
        if deleted {
            self.inner.permissions.invalidate_all();
            info!(role_id = %id, "role deleted, permission cache invalidated");
            self.record_role("delete", id, &name).await;
        }
        Ok(deleted)
    }

    /// How many users hold a role.
    pub async fn member_count(&self, role_id: Uuid) -> Result<i64> {
        Role::member_count(&self.inner.pool, role_id).await
    }

    /// Get all permissions for a role.
    pub async fn get_permissions(&self, role_id: Uuid) -> Result<Vec<String>> {
        Role::get_permissions(&self.inner.pool, role_id).await
    }

    /// Add a permission to a role and invalidate the permission cache.
    pub async fn add_permission(&self, role_id: Uuid, permission: &str) -> Result<()> {
        let permission = permission.to_string();
        self.change_grants(role_id, move |tx| {
            Box::pin(async move { Role::add_permission(tx, role_id, &permission).await })
        })
        .await
    }

    /// Remove a permission from a role and invalidate the permission cache.
    pub async fn remove_permission(&self, role_id: Uuid, permission: &str) -> Result<()> {
        let permission = permission.to_string();
        self.change_grants(role_id, move |tx| {
            Box::pin(async move { Role::remove_permission(tx, role_id, &permission).await })
        })
        .await
    }

    /// Bulk-update permissions for a role.
    ///
    /// Computes the diff between current and desired permissions, applies
    /// adds/removes, and invalidates the permission cache once. The grants
    /// and `role.permissions_changed` commit together.
    pub async fn save_permissions(&self, role_id: Uuid, desired: &[String]) -> Result<()> {
        // The set arithmetic lives on the model, because `config import` needs the
        // same replace semantics and two implementations of it would drift. What
        // this wrapper adds is the cache invalidation, which is a service concern:
        // the import CLI runs in its own process and has no cache to invalidate.
        let desired = desired.to_vec();
        self.change_grants(role_id, move |tx| {
            Box::pin(async move { Role::set_permissions(tx, role_id, &desired).await })
        })
        .await
    }

    /// Update a role's permissions **within a known subset**, leaving any
    /// permission outside `rendered` exactly as it was.
    ///
    /// What the permission grid saves with. See
    /// [`Role::set_permissions_within`] for why a partial view of the
    /// permission universe must not replace the whole set.
    pub async fn save_permissions_within(
        &self,
        role_id: Uuid,
        desired: &[String],
        rendered: &std::collections::HashSet<String>,
    ) -> Result<()> {
        let desired = desired.to_vec();
        let rendered = rendered.clone();
        self.change_grants(role_id, move |tx| {
            Box::pin(
                async move { Role::set_permissions_within(tx, role_id, &desired, &rendered).await },
            )
        })
        .await
    }

    /// Get all roles for a user.
    pub async fn get_user_roles(&self, user_id: Uuid) -> Result<Vec<Role>> {
        Role::get_user_roles(&self.inner.pool, user_id).await
    }

    /// Assign a role to a user and invalidate that user's permission cache.
    /// The assignment and `user.roles_changed` commit together.
    pub async fn assign_to_user(&self, user_id: Uuid, role_id: Uuid) -> Result<()> {
        let mut tx = self.inner.pool.begin().await?;
        let held = Role::get_user_roles(&mut *tx, user_id)
            .await?
            .iter()
            .any(|r| r.id == role_id);
        Role::assign_to_user(&mut *tx, user_id, role_id).await?;
        let name = if held {
            None
        } else {
            Some(membership_event_in(&self.inner.audit, &mut tx, user_id, role_id, "added").await?)
        };
        tx.commit().await?;
        self.inner.permissions.invalidate_user(user_id);
        if let Some(name) = name {
            record_membership(&self.inner.audit, user_id, role_id, &name, "added").await;
        }
        Ok(())
    }

    /// Remove a role from a user and invalidate that user's permission cache.
    /// The removal and `user.roles_changed` commit together.
    pub async fn remove_from_user(&self, user_id: Uuid, role_id: Uuid) -> Result<()> {
        let mut tx = self.inner.pool.begin().await?;
        let held = Role::get_user_roles(&mut *tx, user_id)
            .await?
            .iter()
            .any(|r| r.id == role_id);
        Role::remove_from_user(&mut *tx, user_id, role_id).await?;
        let name = if held {
            Some(
                membership_event_in(&self.inner.audit, &mut tx, user_id, role_id, "removed")
                    .await?,
            )
        } else {
            None
        };
        tx.commit().await?;
        self.inner.permissions.invalidate_user(user_id);
        if let Some(name) = name {
            record_membership(&self.inner.audit, user_id, role_id, &name, "removed").await;
        }
        Ok(())
    }

    /// Get all permissions for a user (aggregated from all their roles).
    pub async fn get_user_permissions(&self, user_id: Uuid) -> Result<Vec<String>> {
        Role::get_user_permissions(&self.inner.pool, user_id).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use crate::models::role::well_known;

    #[test]
    fn well_known_role_ids_are_distinct() {
        assert_ne!(
            well_known::ANONYMOUS_ROLE_ID,
            well_known::AUTHENTICATED_ROLE_ID
        );
    }

    /// The set arithmetic `save_permissions` performs, tested against the real
    /// function.
    ///
    /// Both of these tests used to reimplement the diff in their own bodies, which
    /// meant they asserted that two `HashSet` differences agree with each other
    /// and would have passed with `save_permissions` deleted.
    #[test]
    fn permission_diff_adds_and_revokes() {
        let current = strings(&["read", "write", "delete"]);
        let desired = strings(&["read", "execute"]);

        let (to_add, to_remove) = crate::models::role::permission_diff(&current, &desired);

        assert_eq!(to_add, strings(&["execute"]));
        assert_eq!(
            to_remove,
            strings(&["delete", "write"]),
            "both dropped permissions are revoked, and the order is deterministic"
        );
    }

    #[test]
    fn permission_diff_of_an_unchanged_set_is_empty() {
        let current = strings(&["read", "write"]);
        let desired = strings(&["write", "read"]);

        let (to_add, to_remove) = crate::models::role::permission_diff(&current, &desired);

        assert!(to_add.is_empty(), "no permissions should be added");
        assert!(to_remove.is_empty(), "no permissions should be removed");
    }

    /// Replace semantics: an empty desired set revokes everything.
    #[test]
    fn permission_diff_to_an_empty_set_revokes_all() {
        let current = strings(&["read", "write"]);
        let (to_add, to_remove) = crate::models::role::permission_diff(&current, &[]);
        assert!(to_add.is_empty());
        assert_eq!(to_remove, strings(&["read", "write"]));
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_string()).collect()
    }
}
