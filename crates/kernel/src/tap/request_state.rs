//! Per-request state for WASM plugin execution.
//!
//! Each tap invocation gets a fresh `RequestState` attached to the Wasmtime `Store`.
//! This provides plugins with access to request context, user info, and services.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use moka::sync::Cache;
use sqlx::PgPool;
use uuid::Uuid;

use crate::cache::CacheLayer;
use crate::lockout::LockoutService;
use crate::plugin::PluginRuntime;
use crate::services::ai_provider::AiProviderService;
use crate::services::ai_token_budget::AiTokenBudgetService;

/// Build a field-access decision cache with the canonical FR-8 sizing
/// (10k entries, 5-minute TTL — design §3).
///
/// One shared instance is created in `AppState`, threaded through both
/// [`RequestServices`] and `ItemService`, so a plugin config write
/// (`variables::set`) can flush the *same* cache the read paths consult
/// (design amendment α — config-driven field-rule changes take effect on the
/// next request instead of riding the ≤5-minute TTL).
pub fn new_field_access_cache() -> Cache<String, bool> {
    Cache::builder()
        .max_capacity(10_000)
        .time_to_live(Duration::from_secs(300))
        .build()
}

/// User context for the current request.
#[derive(Debug, Clone)]
pub struct UserContext {
    /// User ID (Uuid::nil() for anonymous).
    pub id: Uuid,
    /// Whether the user is authenticated.
    pub authenticated: bool,
    /// Cached permissions for the user.
    pub permissions: Vec<String>,
    /// Kernel-internal background principal marker (P11c / D-40).
    ///
    /// `true` only for the cron and queue-worker dispatch contexts the kernel
    /// builds through [`RequestServices::for_background`] — set exclusively by
    /// [`UserContext::background`]. **No web/session/auth middleware path
    /// constructs or inherits this marker** (the structural invariant that makes
    /// the background-AI principal safe): every request-handling context is built
    /// with [`UserContext::anonymous`] or [`UserContext::authenticated`], both of
    /// which leave this `false`.
    ///
    /// The marker authorizes background AI (`ai-request`) **only when the calling
    /// plugin also holds the `ai_background` manifest capability** (D-41); it does
    /// **not** grant the human `use ai` permission plane, which stays the sole
    /// gate for web/user AI calls.
    background: bool,
}

impl UserContext {
    /// Create context for anonymous user.
    pub fn anonymous() -> Self {
        Self {
            id: Uuid::nil(),
            authenticated: false,
            permissions: Vec::new(),
            background: false,
        }
    }

    /// Create context for authenticated user.
    pub fn authenticated(id: Uuid, permissions: Vec<String>) -> Self {
        Self {
            id,
            authenticated: true,
            permissions,
            background: false,
        }
    }

    /// Create the kernel-internal background principal (P11c / D-40).
    ///
    /// Carried only by cron and queue-worker dispatch (the `for_background`
    /// contexts). It is **not** authenticated and holds **no** permissions — it
    /// carries no human identity and does not satisfy the `use ai` permission
    /// plane. Its sole authority is to authorize `ai-request` from a plugin that
    /// declared the `ai_background` capability. Never construct this from a
    /// web/session/auth path.
    pub fn background() -> Self {
        Self {
            id: Uuid::nil(),
            authenticated: false,
            permissions: Vec::new(),
            background: true,
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

    /// Whether this is the kernel-internal background principal (P11c / D-40).
    ///
    /// `true` only for contexts built by [`UserContext::background`]; every
    /// web-constructed context returns `false`.
    pub fn is_background(&self) -> bool {
        self.background
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
/// All fields are `Clone`-cheap (PgPool wraps Arc, CacheLayer wraps Arc, etc.).
#[derive(Clone)]
pub struct RequestServices {
    /// Database connection pool.
    pub db: PgPool,
    /// Two-tier cache (Moka L1 + Redis L2). None in test/background contexts.
    pub cache: Option<CacheLayer>,
    /// Lockout service for rate limiting (None in background/cron contexts).
    pub lockout: Option<Arc<LockoutService>>,
    /// AI provider service for making AI requests from plugins.
    pub ai_providers: Option<Arc<AiProviderService>>,
    /// Token budget service for tracking and enforcing AI usage limits.
    pub ai_budgets: Option<Arc<AiTokenBudgetService>>,
    /// Shared HTTP client for outbound requests from plugins.
    pub http: reqwest::Client,
    /// Plugin runtime handle, enabling plugin-to-plugin invocation (FR-4a) from
    /// host functions. `None` in serviceless/test contexts; populated via
    /// [`RequestServices::with_plugin_runtime`] on production dispatch paths. When
    /// absent, `plugin-api::invoke` resolves no targets.
    pub plugin_runtime: Option<Arc<PluginRuntime>>,
    /// Shared field-access decision cache (design amendment α). The **same**
    /// instance `ItemService` reads/writes on the field-access read paths, so the
    /// `variables::set` host function can flush it on a plugin config write and
    /// have the next request reflect the change. Each constructor defaults to a
    /// fresh cache; production overrides it with the one shared instance via
    /// [`RequestServices::with_field_access_cache`] on the `AppState` template.
    pub field_access_cache: Arc<Cache<String, bool>>,
}

impl RequestServices {
    /// Create services for tap dispatch from request-handling contexts.
    ///
    /// Includes the cache layer for plugin cache host functions.
    pub fn for_request(
        db: PgPool,
        cache: Option<CacheLayer>,
        ai_providers: Option<Arc<AiProviderService>>,
        ai_budgets: Option<Arc<AiTokenBudgetService>>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            db,
            cache,
            lockout: None,
            ai_providers,
            ai_budgets,
            http,
            plugin_runtime: None,
            field_access_cache: Arc::new(new_field_access_cache()),
        }
    }

    /// Create services for background tasks (cron, batch) — no lockout or cache.
    pub fn for_background(
        db: PgPool,
        ai_providers: Option<Arc<AiProviderService>>,
        ai_budgets: Option<Arc<AiTokenBudgetService>>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            db,
            cache: None,
            lockout: None,
            ai_providers,
            ai_budgets,
            http,
            plugin_runtime: None,
            field_access_cache: Arc::new(new_field_access_cache()),
        }
    }

    /// Attach a plugin runtime handle, enabling plugin-to-plugin invocation
    /// (FR-4a) from this request's host functions. Builder-style: applied on
    /// production dispatch paths after the runtime `Arc` exists.
    #[must_use]
    pub fn with_plugin_runtime(mut self, runtime: Arc<PluginRuntime>) -> Self {
        self.plugin_runtime = Some(runtime);
        self
    }

    /// Attach the shared field-access cache (design amendment α). Builder-style:
    /// the `AppState` template sets the one shared instance here so every
    /// production tap dispatch — and the `variables::set` flush — operate on the
    /// same cache `ItemService` reads.
    #[must_use]
    pub fn with_field_access_cache(mut self, cache: Arc<Cache<String, bool>>) -> Self {
        self.field_access_cache = cache;
        self
    }

    /// Get the plugin runtime handle, if one was attached.
    pub fn plugin_runtime(&self) -> Option<&Arc<PluginRuntime>> {
        self.plugin_runtime.as_ref()
    }
}

impl std::fmt::Debug for RequestServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestServices")
            .field("db", &"PgPool")
            .field("cache", &self.cache.as_ref().map(|_| "CacheLayer"))
            .field("lockout", &self.lockout.as_ref().map(|_| "LockoutService"))
            .field(
                "ai_providers",
                &self.ai_providers.as_ref().map(|_| "AiProviderService"),
            )
            .field(
                "ai_budgets",
                &self.ai_budgets.as_ref().map(|_| "AiTokenBudgetService"),
            )
            .field("http", &"reqwest::Client")
            .field(
                "plugin_runtime",
                &self.plugin_runtime.as_ref().map(|_| "PluginRuntime"),
            )
            .field("field_access_cache", &"Cache<String, bool>")
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
    /// Plugin-to-plugin invocation depth for the FR-4a recursion bound (Story 2.3).
    ///
    /// The originating request runs at depth `0`; `plugin-api::invoke` carries
    /// `parent + 1` into the child's cloned state and rejects a dispatch once it
    /// would reach the kernel's invocation-depth cap (`MAX_INVOCATION_DEPTH`).
    /// Because Story 2.2 clones `RequestState` per call, each frame owns its clone
    /// and there is no shared counter to decrement.
    ///
    /// `pub(crate)` by design: the kernel sets it, and **no host function reads or
    /// resets it**, so a plugin cannot escape the bound. (The `context` map a
    /// plugin *can* write through the request-context host function is a separate
    /// field — the depth deliberately does not live there.)
    pub(crate) invocation_depth: u32,
}

impl RequestState {
    /// Create a new request state with user context and services.
    pub fn new(user: UserContext, services: RequestServices) -> Self {
        Self {
            user,
            context: HashMap::new(),
            services: Some(services),
            invocation_depth: 0,
        }
    }

    /// Create request state without services (for testing).
    pub fn without_services(user: UserContext) -> Self {
        Self {
            user,
            context: HashMap::new(),
            services: None,
            invocation_depth: 0,
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
    fn background_principal_marker_only_from_background_ctor() {
        // P11c / D-40 structural invariant: the background marker is set by
        // `UserContext::background()` and by *no* web-constructible constructor.
        assert!(UserContext::background().is_background());
        assert!(!UserContext::anonymous().is_background());
        assert!(
            !UserContext::authenticated(Uuid::now_v7(), vec!["use ai".to_string()]).is_background(),
            "an authenticated web user — even one holding 'use ai' — is never a background principal"
        );
        assert!(!UserContext::default().is_background());

        // The background principal carries no human identity or permissions: it
        // must not satisfy the `use ai` permission plane on its own.
        let bg = UserContext::background();
        assert!(!bg.authenticated);
        assert!(bg.permissions.is_empty());
        assert!(!bg.has_permission("use ai"));
        assert_eq!(bg.id, Uuid::nil());
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
