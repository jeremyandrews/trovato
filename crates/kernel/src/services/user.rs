//! User service with tap integration and TTL-based caching.
//!
//! Centralizes user CRUD operations with automatic tap invocations
//! for plugin taps (register, update, delete, login, logout) and
//! an in-process cache for `find_by_id` lookups.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use moka::sync::Cache;
use sqlx::PgPool;
use tracing::info;
use uuid::Uuid;

use crate::models::{CreateUser, UpdateUser, User};
use crate::tap::{RequestState, TapDispatcher, UserContext};

/// Maximum entries in the user cache.
const MAX_CAPACITY: u64 = 10_000;

/// Service for user CRUD operations with tap integration and caching.
///
/// Always present in [`AppState`](crate::state::AppState) as `Arc<UserService>`.
/// The cache deduplicates `find_by_id` lookups that are called repeatedly
/// across route helpers (`require_login`, `require_admin`, author info in
/// list pages, etc.).
///
/// Entries expire automatically after the configured TTL so external changes
/// (direct SQL, second server instance) become visible without a restart.
#[derive(Clone)]
pub struct UserService {
    inner: Arc<UserServiceInner>,
}

struct UserServiceInner {
    pool: PgPool,
    dispatcher: Arc<TapDispatcher>,
    cache: Cache<Uuid, User>,
}

impl UserService {
    /// Create a new user service.
    pub fn new(pool: PgPool, dispatcher: Arc<TapDispatcher>, ttl: Duration) -> Self {
        Self {
            inner: Arc::new(UserServiceInner {
                pool,
                dispatcher,
                cache: Cache::builder()
                    .max_capacity(MAX_CAPACITY)
                    .time_to_live(ttl)
                    .build(),
            }),
        }
    }

    /// Find a user by ID (cached).
    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<User>> {
        // Check cache first
        if let Some(user) = self.inner.cache.get(&id) {
            return Ok(Some(user));
        }

        let user = User::find_by_id(&self.inner.pool, id).await?;

        if let Some(ref u) = user {
            self.inner.cache.insert(id, u.clone());
        }

        Ok(user)
    }

    /// Find a user by username (not cached — used for uniqueness checks).
    pub async fn find_by_name(&self, name: &str) -> Result<Option<User>> {
        User::find_by_name(&self.inner.pool, name).await
    }

    /// Find a user by email (not cached — used for uniqueness checks).
    pub async fn find_by_mail(&self, mail: &str) -> Result<Option<User>> {
        User::find_by_mail(&self.inner.pool, mail).await
    }

    /// Create a new active user with `tap_user_register` invocation.
    pub async fn create(&self, input: CreateUser, acting_user: &UserContext) -> Result<User> {
        let user = User::create(&self.inner.pool, input).await?;
        self.inner.cache.insert(user.id, user.clone());
        self.dispatch_tap("tap_user_register", user.id, acting_user)
            .await;
        info!(user_id = %user.id, name = %user.name, "user created");
        Ok(user)
    }

    /// Create a new user with a specific status and `tap_user_register` invocation.
    ///
    /// Use `status = 0` for inactive accounts pending email verification.
    pub async fn create_with_status(
        &self,
        input: CreateUser,
        status: i16,
        acting_user: &UserContext,
    ) -> Result<User> {
        let user = User::create_with_status(&self.inner.pool, input, status).await?;
        self.inner.cache.insert(user.id, user.clone());
        self.dispatch_tap("tap_user_register", user.id, acting_user)
            .await;
        info!(user_id = %user.id, name = %user.name, status, "user created with status");
        Ok(user)
    }

    /// Update a user with `tap_user_update` invocation.
    pub async fn update(
        &self,
        id: Uuid,
        input: UpdateUser,
        acting_user: &UserContext,
    ) -> Result<Option<User>> {
        let user = User::update(&self.inner.pool, id, input).await?;

        if let Some(ref u) = user {
            self.inner.cache.insert(id, u.clone());
            self.dispatch_tap("tap_user_update", id, acting_user).await;
            info!(user_id = %id, "user updated");
        }

        Ok(user)
    }

    /// Update a user's password with `tap_user_update` invocation.
    pub async fn update_password(
        &self,
        id: Uuid,
        new_password: &str,
        acting_user: &UserContext,
    ) -> Result<bool> {
        let updated = User::update_password(&self.inner.pool, id, new_password).await?;

        if updated {
            self.invalidate(id);
            self.dispatch_tap("tap_user_update", id, acting_user).await;
            info!(user_id = %id, "user password updated");
        }

        Ok(updated)
    }

    /// Delete a user with `tap_user_delete` invocation (before delete).
    pub async fn delete(&self, id: Uuid, acting_user: &UserContext) -> Result<bool> {
        // Dispatch tap before deletion
        self.dispatch_tap("tap_user_delete", id, acting_user).await;

        let deleted = User::delete(&self.inner.pool, id).await?;
        if deleted {
            self.invalidate(id);
            info!(user_id = %id, "user deleted");
        }

        Ok(deleted)
    }

    /// Record a successful login: update timestamps and dispatch `tap_user_login`.
    ///
    /// The tap fires with an empty-permission `UserContext` because this runs
    /// during the login flow before the session is fully established. Plugins
    /// that need the user's permissions should load them from the database.
    pub async fn record_login(&self, user: &User) -> Result<()> {
        User::touch_login(&self.inner.pool, user.id).await?;
        self.invalidate(user.id);
        let user_ctx = UserContext::authenticated(user.id, vec![]);
        self.dispatch_tap("tap_user_login", user.id, &user_ctx)
            .await;
        Ok(())
    }

    /// Record a logout: dispatch `tap_user_logout`.
    ///
    /// The tap fires with an empty-permission `UserContext` because the
    /// session is being destroyed. Plugins should use the user ID for
    /// any post-logout processing.
    pub async fn record_logout(&self, user_id: Uuid) -> Result<()> {
        let user_ctx = UserContext::authenticated(user_id, vec![]);
        self.dispatch_tap("tap_user_logout", user_id, &user_ctx)
            .await;
        Ok(())
    }

    /// List all users.
    pub async fn list(&self) -> Result<Vec<User>> {
        User::list(&self.inner.pool).await
    }

    /// List users with pagination.
    pub async fn list_paginated(&self, limit: i64, offset: i64) -> Result<Vec<User>> {
        User::list_paginated(&self.inner.pool, limit, offset).await
    }

    /// Count all users.
    pub async fn count(&self) -> Result<i64> {
        User::count(&self.inner.pool).await
    }

    /// Update the user's last access time (no tap, no cache invalidation).
    pub async fn touch_access(&self, id: Uuid) -> Result<()> {
        User::touch_access(&self.inner.pool, id).await
    }

    /// Verify a password against a user's hash.
    pub fn verify_password(user: &User, password: &str) -> bool {
        user.verify_password(password)
    }

    /// Invalidate cached user.
    pub fn invalidate(&self, id: Uuid) {
        self.inner.cache.invalidate(&id);
    }

    /// Clear all cached users.
    pub fn clear_cache(&self) {
        self.inner.cache.invalidate_all();
    }

    /// Get cache size.
    pub fn cache_size(&self) -> usize {
        self.inner.cache.entry_count() as usize
    }

    /// Dispatch a user tap hook with standard `{ "user_id": "..." }` payload.
    async fn dispatch_tap(&self, tap_name: &str, user_id: Uuid, acting_user: &UserContext) {
        let json = serde_json::json!({ "user_id": user_id.to_string() });
        let state = RequestState::without_services(acting_user.clone());
        let _ = self
            .inner
            .dispatcher
            .dispatch(tap_name, &json.to_string(), state)
            .await;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn user_tap_payload_format() {
        let id = Uuid::now_v7();
        let json = serde_json::json!({ "user_id": id.to_string() });
        let s = json.to_string();
        assert!(s.contains(&id.to_string()));
    }

    fn make_test_user(id: Uuid, name: &str) -> User {
        User {
            id,
            name: name.to_string(),
            pass: String::new(),
            mail: format!("{name}@test.com"),
            status: 1,
            is_admin: false,
            created: chrono::Utc::now(),
            access: None,
            login: None,
            timezone: None,
            language: None,
            data: serde_json::Value::Null,
        }
    }

    #[test]
    fn cache_insert_and_retrieve() {
        let cache: Cache<Uuid, User> = Cache::builder().max_capacity(100).build();
        let id = Uuid::now_v7();
        let user = make_test_user(id, "test");

        cache.insert(id, user);
        cache.run_pending_tasks();
        assert_eq!(cache.entry_count(), 1);

        let cached = cache.get(&id).unwrap();
        assert_eq!(cached.name, "test");
        assert_eq!(cached.mail, "test@test.com");
    }

    #[test]
    fn cache_invalidate_removes_entry() {
        let cache: Cache<Uuid, User> = Cache::builder().max_capacity(100).build();
        let id = Uuid::now_v7();
        cache.insert(id, make_test_user(id, "test"));
        assert!(cache.get(&id).is_some());

        cache.invalidate(&id);
        assert!(cache.get(&id).is_none());
    }

    #[test]
    fn cache_clear_removes_all() {
        let cache: Cache<Uuid, User> = Cache::builder().max_capacity(100).build();
        for i in 0..5 {
            let id = Uuid::now_v7();
            cache.insert(id, make_test_user(id, &format!("user_{i}")));
        }
        cache.run_pending_tasks();
        assert_eq!(cache.entry_count(), 5);

        cache.invalidate_all();
        // Moka invalidate_all is lazy; run_pending_tasks ensures eviction
        cache.run_pending_tasks();
        assert_eq!(cache.entry_count(), 0);
    }

    #[test]
    fn cache_update_replaces_entry() {
        let cache: Cache<Uuid, User> = Cache::builder().max_capacity(100).build();
        let id = Uuid::now_v7();
        cache.insert(id, make_test_user(id, "original"));
        assert_eq!(cache.get(&id).unwrap().name, "original");

        // Simulate update: insert new version
        cache.insert(id, make_test_user(id, "updated"));
        assert_eq!(cache.get(&id).unwrap().name, "updated");
    }
}
