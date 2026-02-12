//! Permission checking service with DashMap-based caching.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::role::well_known;
use crate::models::{Role, User};

/// Permission cache entry.
#[derive(Debug, Clone)]
struct CachedPermissions {
    permissions: HashSet<String>,
}

/// Permission service with fast DashMap-based lookups.
#[derive(Clone)]
pub struct PermissionService {
    inner: Arc<PermissionServiceInner>,
}

struct PermissionServiceInner {
    /// Cache of user_id -> permissions.
    user_cache: DashMap<Uuid, CachedPermissions>,

    /// Database pool for cache misses.
    pool: PgPool,
}

impl PermissionService {
    /// Create a new permission service.
    pub fn new(pool: PgPool) -> Self {
        Self {
            inner: Arc::new(PermissionServiceInner {
                user_cache: DashMap::new(),
                pool,
            }),
        }
    }

    /// Check if a user has a specific permission.
    ///
    /// - Admin users always return true.
    /// - Anonymous users check only the anonymous role.
    /// - Authenticated users check their assigned roles + authenticated role.
    pub async fn user_has_permission(&self, user: &User, permission: &str) -> Result<bool> {
        // Admins have all permissions
        if user.is_admin {
            return Ok(true);
        }

        // Check cache first
        if let Some(cached) = self.inner.user_cache.get(&user.id) {
            return Ok(cached.permissions.contains(permission));
        }

        // Cache miss - load from database
        let permissions = self.load_user_permissions(user).await?;
        let has_permission = permissions.contains(permission);

        // Cache the result
        self.inner.user_cache.insert(
            user.id,
            CachedPermissions {
                permissions,
            },
        );

        Ok(has_permission)
    }

    /// Load user permissions from the database.
    async fn load_user_permissions(&self, user: &User) -> Result<HashSet<String>> {
        let mut permissions = HashSet::new();

        if user.is_anonymous() {
            // Anonymous users only get anonymous role permissions
            let anon_perms =
                Role::get_permissions(&self.inner.pool, well_known::ANONYMOUS_ROLE_ID).await?;
            permissions.extend(anon_perms);
        } else {
            // Get user's direct role permissions
            let user_perms = Role::get_user_permissions(&self.inner.pool, user.id).await?;
            permissions.extend(user_perms);

            // All authenticated users also get the authenticated role permissions
            let auth_perms =
                Role::get_permissions(&self.inner.pool, well_known::AUTHENTICATED_ROLE_ID).await?;
            permissions.extend(auth_perms);
        }

        Ok(permissions)
    }

    /// Invalidate the cache for a specific user.
    ///
    /// Call this when a user's roles or permissions change.
    pub fn invalidate_user(&self, user_id: Uuid) {
        self.inner.user_cache.remove(&user_id);
    }

    /// Invalidate the entire cache.
    ///
    /// Call this when role permissions change.
    pub fn invalidate_all(&self) {
        self.inner.user_cache.clear();
    }

    /// Get the number of cached entries (for monitoring).
    pub fn cache_size(&self) -> usize {
        self.inner.user_cache.len()
    }
}
