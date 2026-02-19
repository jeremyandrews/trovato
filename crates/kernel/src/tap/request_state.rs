//! Per-request state for WASM plugin execution.
//!
//! Each tap invocation gets a fresh `RequestState` attached to the Wasmtime `Store`.
//! This provides plugins with access to request context, user info, and services.

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;

use crate::lockout::LockoutService;

/// User context for the current request.
#[derive(Debug, Clone)]
pub struct UserContext {
    /// User ID (Uuid::nil() for anonymous).
    pub id: Uuid,
    /// Whether the user is authenticated.
    pub authenticated: bool,
    /// Cached permissions for the user.
    pub permissions: Vec<String>,
}

impl UserContext {
    /// Create context for anonymous user.
    pub fn anonymous() -> Self {
        Self {
            id: Uuid::nil(),
            authenticated: false,
            permissions: Vec::new(),
        }
    }

    /// Create context for authenticated user.
    pub fn authenticated(id: Uuid, permissions: Vec<String>) -> Self {
        Self {
            id,
            authenticated: true,
            permissions,
        }
    }

    /// Check if user has a specific permission.
    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p == permission)
    }

    /// Check if user is admin.
    pub fn is_admin(&self) -> bool {
        self.has_permission("administer site")
    }
}

impl Default for UserContext {
    fn default() -> Self {
        Self::anonymous()
    }
}

/// Services available to plugins during tap execution.
///
/// Services are shared via Arc for efficient cloning into each Store.
#[derive(Clone)]
pub struct RequestServices {
    /// Database connection pool.
    pub db: PgPool,
    /// Lockout service for rate limiting (None in background/cron contexts).
    pub lockout: Option<Arc<LockoutService>>,
    // Future: Add cache, template engine, etc.
}

impl RequestServices {
    /// Create services for background tasks (cron, batch) — no lockout needed.
    pub fn for_background(db: PgPool) -> Self {
        Self { db, lockout: None }
    }
}

impl std::fmt::Debug for RequestServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestServices")
            .field("db", &"PgPool")
            .field("lockout", &self.lockout.as_ref().map(|_| "LockoutService"))
            .finish()
    }
}

/// Per-request state passed to WASM Store.
///
/// Created fresh for each tap invocation. Provides:
/// - User context (ID, authentication status, permissions)
/// - Request-scoped key-value context
/// - Access to shared services (db, cache, etc.)
///
/// # Example
///
/// ```ignore
/// let state = RequestState::new(user_context, services);
/// let mut store = Store::new(&engine, state);
/// // Execute WASM with this store
/// ```
#[derive(Debug, Clone)]
pub struct RequestState {
    /// User context for this request.
    pub user: UserContext,
    /// Per-request key-value store for plugin communication.
    pub context: HashMap<String, String>,
    /// Shared services.
    services: Option<RequestServices>,
}

impl RequestState {
    /// Create a new request state with user context and services.
    pub fn new(user: UserContext, services: RequestServices) -> Self {
        Self {
            user,
            context: HashMap::new(),
            services: Some(services),
        }
    }

    /// Create request state without services (for testing).
    pub fn without_services(user: UserContext) -> Self {
        Self {
            user,
            context: HashMap::new(),
            services: None,
        }
    }

    /// Get the database pool.
    ///
    /// # Panics
    ///
    /// Panics if services were not provided (test mode).
    #[allow(clippy::expect_used)]
    pub fn db(&self) -> &PgPool {
        &self.services.as_ref().expect("services not initialized").db
    }

    /// Get the lockout service (None in background/cron contexts or test mode).
    pub fn lockout(&self) -> Option<&LockoutService> {
        self.services.as_ref().and_then(|s| s.lockout.as_deref())
    }

    /// Check if services are available.
    pub fn has_services(&self) -> bool {
        self.services.is_some()
    }

    /// Get shared services (None in test mode or serviceless contexts).
    pub fn services(&self) -> Option<&RequestServices> {
        self.services.as_ref()
    }

    /// Get a context value.
    pub fn get_context(&self, key: &str) -> Option<&str> {
        self.context.get(key).map(|s| s.as_str())
    }

    /// Set a context value.
    pub fn set_context(&mut self, key: String, value: String) {
        self.context.insert(key, value);
    }

    /// Get current user ID as string (for WASM interop).
    pub fn user_id_string(&self) -> String {
        self.user.id.to_string()
    }
}

impl Default for RequestState {
    fn default() -> Self {
        Self::without_services(UserContext::anonymous())
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_user_context() {
        let ctx = UserContext::anonymous();
        assert_eq!(ctx.id, Uuid::nil());
        assert!(!ctx.authenticated);
        assert!(ctx.permissions.is_empty());
        assert!(!ctx.has_permission("admin"));
    }

    #[test]
    fn authenticated_user_context() {
        let id = Uuid::new_v4();
        let perms = vec!["admin".to_string(), "edit".to_string()];
        let ctx = UserContext::authenticated(id, perms);

        assert_eq!(ctx.id, id);
        assert!(ctx.authenticated);
        assert!(ctx.has_permission("admin"));
        assert!(ctx.has_permission("edit"));
        assert!(!ctx.has_permission("delete"));
    }

    #[test]
    fn request_state_default() {
        let state = RequestState::default();
        assert_eq!(state.user.id, Uuid::nil());
        assert!(!state.user.authenticated);
        assert!(!state.has_services());
    }

    #[test]
    fn request_state_context() {
        let mut state = RequestState::default();
        assert!(state.get_context("foo").is_none());

        state.set_context("foo".to_string(), "bar".to_string());
        assert_eq!(state.get_context("foo"), Some("bar"));
    }

    #[test]
    fn request_state_user_id_string() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let ctx = UserContext::authenticated(id, vec![]);
        let state = RequestState::without_services(ctx);

        assert_eq!(
            state.user_id_string(),
            "550e8400-e29b-41d4-a716-446655440000"
        );
    }
}
