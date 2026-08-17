//! Permission checking service with TTL-based caching.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use moka::sync::Cache;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::role::well_known;
use crate::models::{Role, User};

/// Maximum entries in the permission cache.
const MAX_CAPACITY: u64 = 10_000;

/// Permission cache entry.
#[derive(Debug, Clone)]
struct CachedPermissions {
    permissions: HashSet<String>,
}

/// Permission service with TTL-based cached lookups.
#[derive(Clone)]
pub struct PermissionService {
    inner: Arc<PermissionServiceInner>,
}

struct PermissionServiceInner {
    /// Cache of user_id -> permissions (TTL-bounded).
    user_cache: Cache<Uuid, CachedPermissions>,

    /// Database pool for cache misses.
    pool: PgPool,
}

impl PermissionService {
    /// Create a new permission service.
    pub fn new(pool: PgPool, ttl: Duration) -> Self {
        Self {
            inner: Arc::new(PermissionServiceInner {
                user_cache: Cache::builder()
                    .max_capacity(MAX_CAPACITY)
                    .time_to_live(ttl)
                    .build(),
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

        Ok(self.user_permissions(user).await?.contains(permission))
    }

    /// All permissions for a user, served from the TTL cache when it is warm.
    ///
    /// The same set [`load_user_permissions`](Self::load_user_permissions)
    /// returns, memoized for the cache TTL and invalidated by `RoleService` on
    /// every role or permission change (the invariant list in `services::role`
    /// enumerates them).
    ///
    /// Context building goes through here rather than straight to the database
    /// because one request needs the viewer's permissions more than once: a read
    /// handler builds a context for its access check, and `inject_site_context`
    /// builds one to filter navigation. Neither should pay a second round trip
    /// for an answer the other just fetched — a loader that is expensive to call
    /// is a loader someone eventually replaces with a literal permission list.
    pub async fn user_permissions(&self, user: &User) -> Result<HashSet<String>> {
        if let Some(cached) = self.inner.user_cache.get(&user.id) {
            return Ok(cached.permissions);
        }

        let permissions = self.load_user_permissions(user).await?;
        self.inner.user_cache.insert(
            user.id,
            CachedPermissions {
                permissions: permissions.clone(),
            },
        );

        Ok(permissions)
    }

    /// Load all permissions for a user from the database, bypassing the cache.
    ///
    /// Returns the raw role-based permission set (does **not** include the
    /// implicit admin bypass). Callers building a [`UserContext`](crate::tap::UserContext) for admin
    /// users should add `"administer site"` themselves so that
    /// [`UserContext::is_admin`](crate::tap::UserContext::is_admin) returns `true`.
    pub async fn load_user_permissions(&self, user: &User) -> Result<HashSet<String>> {
        let mut permissions = HashSet::new();

        // Note: Uses Role model directly (not RoleService) because
        // PermissionService is initialized before RoleService in AppState.
        // These are read-only lookups with no cache invalidation needed.
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

    /// Build a request-scoped [`UserContext`](crate::tap::UserContext) carrying
    /// this user's **real** permissions.
    ///
    /// This is the one place that turns a loaded [`User`] into a context, and
    /// every request path should reach a context through it (directly, or
    /// through the `routes::helpers` wrappers that add the session lookup and
    /// the web failure policy). Building a context from a literal permission
    /// list instead is the defect class that broke the front page for every
    /// logged-out visitor: the list looks like a permission model and is not
    /// one.
    ///
    /// Three things it gets right that a literal list cannot:
    ///
    /// - The permissions are the user's own, loaded from their roles.
    /// - An admin additionally carries the `"administer site"` marker, because
    ///   [`load_user_permissions`](Self::load_user_permissions) returns the raw
    ///   role set without the implicit admin bypass, and
    ///   [`UserContext::is_admin`](crate::tap::UserContext::is_admin) is keyed
    ///   on that marker.
    /// - The anonymous user's context is built from
    ///   [`UserContext::anonymous`](crate::tap::UserContext::anonymous) rather
    ///   than `authenticated`, so it can never be mistaken for the kernel
    ///   background principal (P11c / D-40).
    ///
    /// **Fails closed**: a permission-load error is returned, not swallowed
    /// into an empty set. Callers that cannot propagate an error must choose a
    /// policy explicitly and say so — see
    /// [`routes::helpers::permissions_or_deny_all`](crate::routes::helpers::permissions_or_deny_all).
    pub async fn user_context(&self, user: &User) -> Result<crate::tap::UserContext> {
        let permissions = self.user_permissions(user).await?;
        Ok(context_from_permissions(user, permissions))
    }

    /// Invalidate the cache for a specific user.
    ///
    /// Call this when a user's roles or permissions change.
    pub fn invalidate_user(&self, user_id: Uuid) {
        self.inner.user_cache.invalidate(&user_id);
    }

    /// Invalidate the entire cache.
    ///
    /// Call this when role permissions change.
    pub fn invalidate_all(&self) {
        self.inner.user_cache.invalidate_all();
    }

    /// Get the number of cached entries (for monitoring).
    pub fn cache_size(&self) -> usize {
        self.inner.user_cache.entry_count() as usize
    }
}

/// Assemble a [`UserContext`](crate::tap::UserContext) from a user and an
/// already-loaded permission set.
///
/// Split out of [`PermissionService::user_context`] so the callers that hold a
/// permission set and their own failure policy (the web loader, which degrades
/// rather than propagating) assemble the context exactly the same way.
pub fn context_from_permissions(
    user: &User,
    permissions: HashSet<String>,
) -> crate::tap::UserContext {
    use crate::tap::UserContext;

    if user.is_anonymous() {
        // Anonymous web caller carrying the anonymous role's permissions.
        // Built from `anonymous()` so it can never be the kernel background
        // principal (P11c / D-40): that marker is private and set only by
        // `UserContext::background()`.
        let mut ctx = UserContext::anonymous();
        ctx.permissions = permissions.into_iter().collect();
        return ctx;
    }

    let mut permissions: Vec<String> = permissions.into_iter().collect();
    // `load_user_permissions` returns the raw role set without the implicit
    // admin bypass, so an admin needs the marker added for
    // `UserContext::is_admin()` to hold.
    if user.is_admin && !permissions.iter().any(|p| p == "administer site") {
        permissions.push("administer site".to_string());
    }
    UserContext::authenticated(user.id, permissions)
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn user(id: Uuid, is_admin: bool) -> User {
        User {
            id,
            name: "u".to_string(),
            pass: String::new(),
            mail: "u@example.com".to_string(),
            is_admin,
            created: chrono::Utc::now(),
            access: None,
            login: None,
            status: 1,
            timezone: None,
            language: None,
            data: serde_json::Value::Null,
            consent_given: None,
            consent_date: None,
            consent_version: None,
            data_retention_days: None,
        }
    }

    fn perms(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn context_carries_the_users_own_permissions() {
        let ctx = context_from_permissions(
            &user(Uuid::now_v7(), false),
            perms(&["access content", "post comments"]),
        );

        assert!(ctx.authenticated);
        assert!(ctx.has_permission("access content"));
        assert!(ctx.has_permission("post comments"));
        assert!(!ctx.is_admin());
    }

    #[test]
    fn admin_context_keeps_real_permissions_alongside_the_marker() {
        let ctx = context_from_permissions(&user(Uuid::now_v7(), true), perms(&["moderate feeds"]));

        // The marker is what `is_admin()` reads, but it must not be the whole
        // permission set: an admin holds their real permissions too.
        assert!(ctx.is_admin());
        assert!(ctx.has_permission("moderate feeds"));
    }

    #[test]
    fn anonymous_context_is_never_authenticated_or_background() {
        let ctx = context_from_permissions(&user(Uuid::nil(), false), perms(&["access content"]));

        assert!(!ctx.authenticated);
        assert!(!ctx.is_background());
        assert!(ctx.has_permission("access content"));
    }

    #[test]
    fn admin_marker_is_not_duplicated_when_a_role_already_grants_it() {
        let ctx =
            context_from_permissions(&user(Uuid::now_v7(), true), perms(&["administer site"]));

        assert_eq!(
            ctx.permissions
                .iter()
                .filter(|p| *p == "administer site")
                .count(),
            1
        );
    }
}
