//! Application state shared across all handlers.

use std::sync::Arc;

use anyhow::{Context, Result};
use redis::Client as RedisClient;
use sqlx::PgPool;

use crate::config::Config;
use crate::content::{ContentTypeRegistry, ItemService};
use crate::db;
use crate::gather::{CategoryService, GatherService};
use crate::lockout::LockoutService;
use crate::menu::MenuRegistry;
use crate::permissions::PermissionService;
use crate::plugin::{PluginConfig, PluginRuntime};
use crate::tap::{TapDispatcher, TapRegistry};

/// Shared application state.
///
/// Wrapped in Arc internally so Clone is cheap.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

struct AppStateInner {
    /// PostgreSQL connection pool.
    db: PgPool,

    /// Redis client for sessions and caching.
    redis: RedisClient,

    /// Permission service for access control.
    permissions: PermissionService,

    /// Account lockout service.
    lockout: LockoutService,

    /// Plugin runtime.
    plugin_runtime: Arc<PluginRuntime>,

    /// Tap registry.
    tap_registry: Arc<TapRegistry>,

    /// Tap dispatcher.
    tap_dispatcher: Arc<TapDispatcher>,

    /// Menu registry.
    menu_registry: Arc<MenuRegistry>,

    /// Content type registry.
    content_types: Arc<ContentTypeRegistry>,

    /// Item service.
    items: Arc<ItemService>,

    /// Category service.
    categories: Arc<CategoryService>,

    /// Gather service.
    gather: Arc<GatherService>,
}

impl AppState {
    /// Create new application state with database connections.
    pub async fn new(config: &Config) -> Result<Self> {
        // Create PostgreSQL pool
        let db = db::create_pool(config)
            .await
            .context("failed to create database pool")?;

        // Run migrations
        db::run_migrations(&db)
            .await
            .context("failed to run migrations")?;

        // Create Redis client
        let redis = RedisClient::open(config.redis_url.as_str())
            .context("failed to create Redis client")?;

        // Test Redis connection
        let mut conn = redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to connect to Redis")?;

        redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
            .context("Redis PING failed")?;

        // Create permission service
        let permissions = PermissionService::new(db.clone());

        // Create lockout service
        let lockout = LockoutService::new(redis.clone());

        // Create plugin runtime and load plugins
        let plugin_config = PluginConfig::default();
        let mut plugin_runtime = PluginRuntime::new(&plugin_config)
            .context("failed to create plugin runtime")?;

        // Load plugins
        plugin_runtime
            .load_all(&config.plugins_dir)
            .await
            .context("failed to load plugins")?;

        let plugin_runtime = Arc::new(plugin_runtime);

        // Create tap registry
        let tap_registry = Arc::new(TapRegistry::from_plugins(&plugin_runtime));

        // Create tap dispatcher
        let tap_dispatcher = Arc::new(TapDispatcher::new(
            plugin_runtime.clone(),
            tap_registry.clone(),
        ));

        // Create menu registry from plugins by invoking tap_menu
        use crate::tap::{RequestState, UserContext};
        let menu_state = RequestState::without_services(UserContext::anonymous());
        let menu_results = tap_dispatcher.dispatch("tap_menu", "{}", menu_state).await;
        let menu_jsons: Vec<(String, String)> = menu_results
            .into_iter()
            .map(|r| (r.plugin_name, r.output))
            .collect();
        let menu_registry = Arc::new(MenuRegistry::from_tap_results(menu_jsons));

        // Create content type registry
        let content_types = Arc::new(ContentTypeRegistry::new(db.clone()));
        content_types
            .sync_from_plugins(&tap_dispatcher)
            .await
            .context("failed to sync content types")?;

        // Create item service
        let items = Arc::new(ItemService::new(db.clone(), tap_dispatcher.clone()));

        // Create category service
        let categories = CategoryService::new(db.clone());

        // Create gather service and load views
        let gather = GatherService::new(db.clone(), categories.clone());
        gather
            .load_views()
            .await
            .context("failed to load gather views")?;

        Ok(Self {
            inner: Arc::new(AppStateInner {
                db,
                redis,
                permissions,
                lockout,
                plugin_runtime,
                tap_registry,
                tap_dispatcher,
                menu_registry,
                content_types,
                items,
                categories,
                gather,
            }),
        })
    }

    /// Get the database pool.
    pub fn db(&self) -> &PgPool {
        &self.inner.db
    }

    /// Get the Redis client.
    pub fn redis(&self) -> &RedisClient {
        &self.inner.redis
    }

    /// Get the permission service.
    pub fn permissions(&self) -> &PermissionService {
        &self.inner.permissions
    }

    /// Get the lockout service.
    pub fn lockout(&self) -> &LockoutService {
        &self.inner.lockout
    }

    /// Get the plugin runtime.
    pub fn plugin_runtime(&self) -> &Arc<PluginRuntime> {
        &self.inner.plugin_runtime
    }

    /// Get the tap registry.
    pub fn tap_registry(&self) -> &Arc<TapRegistry> {
        &self.inner.tap_registry
    }

    /// Get the tap dispatcher.
    pub fn tap_dispatcher(&self) -> &Arc<TapDispatcher> {
        &self.inner.tap_dispatcher
    }

    /// Get the menu registry.
    pub fn menu_registry(&self) -> &Arc<MenuRegistry> {
        &self.inner.menu_registry
    }

    /// Get the content type registry.
    pub fn content_types(&self) -> &Arc<ContentTypeRegistry> {
        &self.inner.content_types
    }

    /// Get the item service.
    pub fn items(&self) -> &Arc<ItemService> {
        &self.inner.items
    }

    /// Get the category service.
    pub fn categories(&self) -> &Arc<CategoryService> {
        &self.inner.categories
    }

    /// Get the gather service.
    pub fn gather(&self) -> &Arc<GatherService> {
        &self.inner.gather
    }

    /// Check if PostgreSQL is healthy.
    pub async fn postgres_healthy(&self) -> bool {
        db::check_health(&self.inner.db).await
    }

    /// Check if Redis is healthy.
    pub async fn redis_healthy(&self) -> bool {
        let Ok(mut conn) = self.inner.redis.get_multiplexed_async_connection().await else {
            return false;
        };

        redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
            .is_ok()
    }
}
