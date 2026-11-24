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

use crate::audit::{SecurityEvent, SecurityEventKind};
use crate::cache::generation::{CacheKind, bump};
use crate::metrics::{CacheName, Metrics};
use crate::models::{CreateUser, UpdateUser, User};
use crate::services::audit::AuditTrail;
use crate::tap::{RequestServices, RequestState, TapDispatcher, UserContext};

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
    tap_services: RequestServices,
    cache: Cache<Uuid, User>,
    /// Counts `cache` hits and misses.
    metrics: Arc<Metrics>,
    audit: AuditTrail,
}

/// Set `user_id`'s password from the command line, where no site is running,
/// and audit it as every other password change is: `password.changed` (or
/// `password.set`) with `details.via = "cli"`, and a content audit row when
/// `trovato_audit_log` is enabled.
pub async fn reset_password_offline(pool: &PgPool, user_id: Uuid, password: &str) -> Result<bool> {
    let had_password = User::find_by_id(pool, user_id)
        .await?
        .is_some_and(|u| !u.pass.is_empty());
    let updated = User::update_password(pool, user_id, password).await?;
    if updated {
        let audit = AuditTrail::offline(pool.clone()).await;
        audit
            .record(
                "password",
                "user",
                user_id,
                None,
                serde_json::json!({ "via": "cli" }),
            )
            .await;
        let kind = if had_password {
            SecurityEventKind::PasswordChanged
        } else {
            SecurityEventKind::PasswordSet
        };
        audit
            .security(SecurityEvent::new(kind).user(user_id).detail("via", "cli"))
            .await;
    }
    Ok(updated)
}

/// The actor a `UserContext` names, for an audit row: `None` for anonymous.
fn actor_of(acting_user: &UserContext) -> Option<Uuid> {
    acting_user.authenticated.then_some(acting_user.id)
}

impl UserService {
    /// Create a new user service.
    pub fn new(
        pool: PgPool,
        dispatcher: Arc<TapDispatcher>,
        tap_services: RequestServices,
        ttl: Duration,
        metrics: Arc<Metrics>,
        audit: AuditTrail,
    ) -> Self {
        Self {
            inner: Arc::new(UserServiceInner {
                pool,
                dispatcher,
                tap_services,
                cache: Cache::builder()
                    .max_capacity(MAX_CAPACITY)
                    .time_to_live(ttl)
                    .build(),
                metrics,
                audit,
            }),
        }
    }

    /// Find a user by ID (cached).
    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<User>> {
        // Check cache first
        if let Some(user) = self
            .inner
            .metrics
            .cache_lookup(CacheName::User, self.inner.cache.get(&id))
        {
            return Ok(Some(user));
        }

        let user = User::find_by_id(&self.inner.pool, id).await?;

        if let Some(ref u) = user {
            self.inner.cache.insert(id, u.clone());
        }

        Ok(user)
    }

    /// The names of the users among `ids`, in one query whatever their number.
    ///
    /// For pages that name many authors at once (the revision history), where
    /// a cached `find_by_id` per author would be one query per author on a
    /// cold cache.
    pub async fn names_by_ids(
        &self,
        ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, String>> {
        User::names_by_ids(&self.inner.pool, ids).await
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
        self.audit_created(&user, acting_user).await;
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
        self.created_with_status(&user, status, acting_user).await;
        Ok(user)
    }

    /// [`Self::create_with_status`], queueing the mail `mail` builds for the new
    /// account in the same transaction as the insert (BG18): a registration
    /// that rolls back sends nothing, and one that commits always sends.
    ///
    /// # Errors
    ///
    /// When the insert or queueing a message fails; then neither happened.
    pub async fn create_with_status_and_mail(
        &self,
        input: CreateUser,
        status: i16,
        acting_user: &UserContext,
        email: &crate::services::email::EmailService,
        mail: impl FnOnce(&User) -> Vec<crate::services::email::NewMail>,
    ) -> Result<User> {
        // Hash before BEGIN: Argon2 takes a while, and a pooled connection
        // must not sit idle in a transaction meanwhile.
        let pass = crate::models::user::hash_password(&input.password)?;
        let mut tx = self.inner.pool.begin().await?;
        let user = User::insert_hashed(&mut *tx, input, &pass, status).await?;
        for message in mail(&user) {
            email.queue(&mut *tx, message).await?;
        }
        tx.commit().await?;
        email.wake();
        self.created_with_status(&user, status, acting_user).await;
        Ok(user)
    }

    /// What follows a committed insert: cache, `tap_user_register`, audit.
    async fn created_with_status(&self, user: &User, status: i16, acting_user: &UserContext) {
        self.inner.cache.insert(user.id, user.clone());
        self.dispatch_tap("tap_user_register", user.id, acting_user)
            .await;
        info!(user_id = %user.id, name = %user.name, status, "user created with status");
        self.audit_created(user, acting_user).await;
    }

    /// Update a user with `tap_user_update` invocation.
    ///
    /// **`acting_user` is the tap principal, not an authorization subject.** This
    /// method performs no access check of its own: it writes the update and then
    /// dispatches `tap_user_update` with `acting_user` as the request context
    /// every listening plugin sees. Authorization belongs to the caller — the
    /// admin routes gate on `require_admin`, and the self-service routes in
    /// `routes::auth` and `routes::recovery` gate on the session user's identity
    /// or a verified token, then pass that same user as the principal.
    ///
    /// Because the principal reaches plugins, it must carry the acting user's
    /// **real** permissions (`routes::helpers::user_context_for`). Passing
    /// `authenticated(id, vec![])` tells every listener that a permission-less
    /// user acted, which is both false and the thing that makes a plugin's own
    /// permission check deny an entitled user.
    pub async fn update(
        &self,
        id: Uuid,
        input: UpdateUser,
        acting_user: &UserContext,
    ) -> Result<Option<User>> {
        let before = User::find_by_id(&self.inner.pool, id).await?;
        let user = User::update(&self.inner.pool, id, input).await?;

        if let Some(ref u) = user {
            self.inner.cache.insert(id, u.clone());
            self.dispatch_tap("tap_user_update", id, acting_user).await;
            info!(user_id = %id, "user updated");
            self.audit_updated(before.as_ref(), u, acting_user).await;
        }

        Ok(user)
    }

    /// Update a user's password with `tap_user_update` invocation.
    ///
    /// As with [`update`](Self::update), `acting_user` is the tap principal and
    /// not an authorization subject: this method authorizes nothing, and the
    /// caller (password change past a verified current password, or a spent
    /// recovery grant) is what gates it.
    ///
    /// Emits `password.set` when the account had no password and
    /// `password.changed` when it had one, with `via` (`self_service`,
    /// `password_reset`, `recovery`, `admin`) as the event's `details.via`.
    pub async fn update_password(
        &self,
        id: Uuid,
        new_password: &str,
        acting_user: &UserContext,
        via: &str,
    ) -> Result<bool> {
        let had_password = User::find_by_id(&self.inner.pool, id)
            .await?
            .is_some_and(|u| !u.pass.is_empty());
        let updated = User::update_password(&self.inner.pool, id, new_password).await?;

        if updated {
            self.invalidate(id);
            self.dispatch_tap("tap_user_update", id, acting_user).await;
            info!(user_id = %id, "user password updated");
            let kind = if had_password {
                SecurityEventKind::PasswordChanged
            } else {
                SecurityEventKind::PasswordSet
            };
            let audit = &self.inner.audit;
            audit
                .record(
                    "password",
                    "user",
                    id,
                    actor_of(acting_user),
                    serde_json::json!({ "via": via }),
                )
                .await;
            audit
                .security(
                    SecurityEvent::new(kind)
                        .user(id)
                        .maybe_actor(actor_of(acting_user).filter(|a| *a != id))
                        .detail("via", via),
                )
                .await;
        }

        Ok(updated)
    }

    /// Remove a user's password with `tap_user_update` invocation, as
    /// [`update_password`](Self::update_password) changes one.
    ///
    /// Authorizes nothing: the caller decides that the account keeps a way in
    /// without it.
    pub async fn remove_password(&self, id: Uuid, acting_user: &UserContext) -> Result<bool> {
        let removed = User::clear_password(&self.inner.pool, id).await?;

        if removed {
            self.invalidate(id);
            self.dispatch_tap("tap_user_update", id, acting_user).await;
            info!(user_id = %id, "user password removed");
            self.inner
                .audit
                .record(
                    "password_removed",
                    "user",
                    id,
                    actor_of(acting_user),
                    serde_json::json!({}),
                )
                .await;
        }

        Ok(removed)
    }

    /// Delete a user with `tap_user_delete` invocation (before delete).
    ///
    /// The tap fires **before** the row goes, which is the only order that works: a
    /// plugin cleaning up its own user-keyed rows needs the user to still exist while
    /// it does so, and a plugin table with a foreign key onto `users` could not be
    /// cleaned afterwards at all.
    ///
    /// [`User::delete`] reattributes the account's authored content to the anonymous
    /// author inside its own transaction; see its documentation for why that is a
    /// bug fix and not only a policy.
    pub async fn delete(&self, id: Uuid, acting_user: &UserContext) -> Result<bool> {
        // Dispatch tap before deletion
        self.dispatch_tap("tap_user_delete", id, acting_user).await;

        let deleted = User::delete(&self.inner.pool, id).await?;
        if deleted {
            self.invalidate(id);
            // The delete gave the account's items to the anonymous author.
            bump(CacheKind::Items);
            info!(user_id = %id, "user deleted");
            self.inner
                .audit
                .record(
                    "delete",
                    "user",
                    id,
                    actor_of(acting_user),
                    serde_json::json!({}),
                )
                .await;
        }

        Ok(deleted)
    }

    /// How many active administrators the site has, excluding the anonymous
    /// sentinel.
    ///
    /// The guard behind "the last administrator cannot delete their own account": a
    /// site with no administrator cannot be administered back into having one.
    pub async fn active_admin_count(&self) -> Result<i64> {
        User::active_admin_count(&self.inner.pool).await
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

    /// Invalidate a cached user, here and on every other server (which clear
    /// their whole user cache). Call it after the write commits.
    pub fn invalidate(&self, id: Uuid) {
        self.inner.cache.invalidate(&id);
        bump(CacheKind::Users);
    }

    /// Clear all cached users, here and on every other server.
    pub fn clear_cache(&self) {
        self.clear_local();
        bump(CacheKind::Users);
    }

    /// Clear this server's cache only: what another server's bump does here.
    pub fn clear_local(&self) {
        self.inner.cache.invalidate_all();
    }

    /// Get cache size.
    pub fn cache_size(&self) -> usize {
        self.inner.cache.entry_count() as usize
    }

    /// Dispatch a user tap hook with standard `{ "user_id": "..." }` payload.
    /// Record an account created.
    async fn audit_created(&self, user: &User, acting_user: &UserContext) {
        self.inner
            .audit
            .record(
                "create",
                "user",
                user.id,
                actor_of(acting_user),
                serde_json::json!({ "name": user.name, "status": user.status }),
            )
            .await;
    }

    /// Record an account updated, and, as security events, a change to its
    /// blocked state or its administrator flag.
    async fn audit_updated(&self, before: Option<&User>, after: &User, acting_user: &UserContext) {
        let actor = actor_of(acting_user);
        let mut changed = Vec::new();
        if let Some(before) = before {
            if before.name != after.name {
                changed.push("name");
            }
            if before.mail != after.mail {
                changed.push("mail");
            }
            if before.status != after.status {
                changed.push("status");
            }
            if before.is_admin != after.is_admin {
                changed.push("is_admin");
            }
        }
        let audit = &self.inner.audit;
        audit
            .record(
                "update",
                "user",
                after.id,
                actor,
                serde_json::json!({ "name": after.name, "changed": changed }),
            )
            .await;
        let Some(before) = before else { return };
        let other_actor = actor.filter(|a| *a != after.id);
        if before.status != after.status {
            audit
                .security(
                    SecurityEvent::new(SecurityEventKind::UserStatusChanged)
                        .user(after.id)
                        .maybe_actor(other_actor)
                        .detail("from", before.status)
                        .detail("to", after.status)
                        .detail("blocked", after.status == 0),
                )
                .await;
        }
        if before.is_admin != after.is_admin {
            audit
                .security(
                    SecurityEvent::new(SecurityEventKind::UserRolesChanged)
                        .user(after.id)
                        .maybe_actor(other_actor)
                        .detail("change", if after.is_admin { "added" } else { "removed" })
                        .detail("role", "administrator"),
                )
                .await;
        }
    }

    async fn dispatch_tap(&self, tap_name: &str, user_id: Uuid, acting_user: &UserContext) {
        let json = serde_json::json!({ "user_id": user_id.to_string() });
        let state = RequestState::new(acting_user.clone(), self.inner.tap_services.clone());
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
            consent_given: None,
            consent_date: None,
            consent_version: None,
            data_retention_days: None,
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
