//! Role service with permission cache invalidation.
//!
//! Wraps role and permission CRUD operations, ensuring that the
//! [`PermissionService`](crate::permissions::PermissionService) cache is
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
        let current = Role::get_permissions(&self.inner.pool, role_id).await?;
        let current_set: std::collections::HashSet<&str> =
            current.iter().map(|s| s.as_str()).collect();
        let desired_set: std::collections::HashSet<&str> =
            desired.iter().map(|s| s.as_str()).collect();

        // Add new permissions
        for perm in &desired_set {
            if !current_set.contains(perm) {
                Role::add_permission(&self.inner.pool, role_id, perm).await?;
            }
        }

        // Remove revoked permissions
        for perm in &current_set {
            if !desired_set.contains(perm) {
                Role::remove_permission(&self.inner.pool, role_id, perm).await?;
            }
        }

        // Single invalidation for the batch
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
