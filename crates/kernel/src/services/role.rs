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

use crate::models::Role;
use crate::permissions::PermissionService;

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
}

impl RoleService {
    /// Create a new role service.
    pub fn new(pool: PgPool, permissions: PermissionService) -> Self {
        Self {
            inner: Arc::new(RoleServiceInner { pool, permissions }),
        }
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

    /// Create a new role.
    pub async fn create(&self, name: &str) -> Result<Role> {
        let role = Role::create(&self.inner.pool, name).await?;
        info!(role_id = %role.id, name = %role.name, "role created");
        Ok(role)
    }

    /// Update a role's name.
    pub async fn update(&self, id: Uuid, name: &str) -> Result<Option<Role>> {
        let role = Role::update(&self.inner.pool, id, name).await?;
        if let Some(ref r) = role {
            info!(role_id = %r.id, name = %r.name, "role updated");
        }
        Ok(role)
    }

    /// Delete a role.
    ///
    /// Prevents deletion of well-known roles (anonymous, authenticated).
    /// Invalidates the entire permission cache because any user with this
    /// role will have different effective permissions after deletion.
    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        let deleted = Role::delete(&self.inner.pool, id).await?;
        if deleted {
            self.inner.permissions.invalidate_all();
            info!(role_id = %id, "role deleted, permission cache invalidated");
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
        Role::add_permission(&self.inner.pool, role_id, permission).await?;
        self.inner.permissions.invalidate_all();
        Ok(())
    }

    /// Remove a permission from a role and invalidate the permission cache.
    pub async fn remove_permission(&self, role_id: Uuid, permission: &str) -> Result<()> {
        Role::remove_permission(&self.inner.pool, role_id, permission).await?;
        self.inner.permissions.invalidate_all();
        Ok(())
    }

    /// Bulk-update permissions for a role.
    ///
    /// Computes the diff between current and desired permissions, applies
    /// adds/removes, and invalidates the permission cache once.
    pub async fn save_permissions(&self, role_id: Uuid, desired: &[String]) -> Result<()> {
        // The set arithmetic lives on the model, because `config import` needs the
        // same replace semantics and two implementations of it would drift. What
        // this wrapper adds is the cache invalidation, which is a service concern:
        // the import CLI runs in its own process and has no cache to invalidate.
        Role::set_permissions(&self.inner.pool, role_id, desired).await?;
        self.inner.permissions.invalidate_all();
        Ok(())
    }

    /// Get all roles for a user.
    pub async fn get_user_roles(&self, user_id: Uuid) -> Result<Vec<Role>> {
        Role::get_user_roles(&self.inner.pool, user_id).await
    }

    /// Assign a role to a user and invalidate that user's permission cache.
    pub async fn assign_to_user(&self, user_id: Uuid, role_id: Uuid) -> Result<()> {
        Role::assign_to_user(&self.inner.pool, user_id, role_id).await?;
        self.inner.permissions.invalidate_user(user_id);
        Ok(())
    }

    /// Remove a role from a user and invalidate that user's permission cache.
    pub async fn remove_from_user(&self, user_id: Uuid, role_id: Uuid) -> Result<()> {
        Role::remove_from_user(&self.inner.pool, user_id, role_id).await?;
        self.inner.permissions.invalidate_user(user_id);
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
