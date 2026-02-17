//! Application state shared across all handlers.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use redis::Client as RedisClient;
use sqlx::PgPool;

use tracing::info;

use crate::middleware::language::{
    AcceptLanguageNegotiator, LanguageNegotiator, UrlPrefixNegotiator,
};

use crate::batch::BatchService;
use crate::cache::CacheLayer;
use crate::config::Config;
use crate::config_storage::{ConfigStorage, DirectConfigStorage, StageAwareConfigStorage};
use crate::content::{ContentTypeRegistry, ItemService};
use crate::cron::CronService;
use crate::db;
use crate::file::{FileService, LocalFileStorage};
use crate::form::FormService;
use crate::gather::{
    CategoryService, GatherExtensionDeclaration, GatherExtensionRegistry, GatherService,
};
use crate::lockout::LockoutService;
use crate::menu::MenuRegistry;
use crate::metrics::Metrics;
use crate::middleware::{RateLimitConfig, RateLimiter};
use crate::permissions::PermissionService;
use crate::plugin::{
    PluginConfig, PluginRuntime, migration as plugin_migration, status as plugin_status,
};
use crate::search::SearchService;
use crate::stage::StageService;
use crate::tap::{TapDispatcher, TapRegistry};
use crate::theme::ThemeEngine;

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

    /// Two-tier cache layer (Moka L1 + Redis L2).
    cache: CacheLayer,

    /// Configuration storage for all config entities.
    /// All config reads/writes MUST go through this interface.
    config_storage: Arc<dyn ConfigStorage>,

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

    /// Search service for full-text search.
    search: Arc<SearchService>,

    /// Theme engine for template rendering.
    theme: Arc<ThemeEngine>,

    /// Form service for form handling.
    forms: Arc<FormService>,

    /// File service for uploads.
    files: Arc<FileService>,

    /// Cron service for scheduled operations.
    cron: Arc<CronService>,

    /// Prometheus metrics.
    metrics: Arc<Metrics>,

    /// Rate limiter.
    rate_limiter: Arc<RateLimiter>,

    /// Batch operations service.
    batch: Arc<BatchService>,

    /// Stage service for publish operations.
    stage: Arc<StageService>,

    /// Language negotiator chain (sorted by priority descending).
    ///
    /// Frozen at startup: adding/removing languages requires a restart.
    /// Each negotiator also holds its own snapshot of known languages.
    language_negotiators: Vec<Arc<dyn LanguageNegotiator>>,

    /// Known language codes (loaded from DB at startup).
    ///
    /// Frozen at startup: adding/removing languages requires a restart.
    known_languages: Vec<String>,

    /// Default language code (loaded from DB at startup).
    ///
    /// Frozen at startup: changing the default language requires a restart.
    default_language: String,
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

        // Create config storage
        // This is the central interface for all config entity access.
        // Post-MVP, we can swap this with StageAwareConfigStorage for stage awareness.
        let config_storage: Arc<dyn ConfigStorage> = Arc::new(DirectConfigStorage::new(db.clone()));

        // Create permission service
        let permissions = PermissionService::new(db.clone());

        // Create lockout service
        let lockout = LockoutService::new(redis.clone());

        // Discover plugins on disk (parse info.toml without compiling WASM)
        let discovered = PluginRuntime::discover_plugins(&config.plugins_dir);

        // Auto-install any new plugins into plugin_status table
        let discovered_pairs: Vec<(&str, &str)> = discovered
            .iter()
            .map(|(name, (info, _))| (name.as_str(), info.version.as_str()))
            .collect();
        let new_count = plugin_status::auto_install_new_plugins(&db, &discovered_pairs)
            .await
            .context("failed to auto-install new plugins")?;
        if new_count > 0 {
            info!(count = new_count, "auto-installed new plugins");
        }

        // Get enabled plugin set
        let enabled_names = plugin_status::get_enabled_names(&db)
            .await
            .context("failed to get enabled plugins")?;
        let enabled_set: std::collections::HashSet<String> = enabled_names.into_iter().collect();

        // Create plugin runtime and load only enabled plugins
        let plugin_config = PluginConfig::default();
        let mut plugin_runtime =
            PluginRuntime::new(&plugin_config).context("failed to create plugin runtime")?;

        plugin_runtime
            .load_enabled(&config.plugins_dir, &enabled_set)
            .await
            .context("failed to load plugins")?;

        // Run pending plugin migrations for enabled plugins
        let enabled_discovered: std::collections::HashMap<
            String,
            (crate::plugin::PluginInfo, std::path::PathBuf),
        > = discovered
            .into_iter()
            .filter(|(name, _)| enabled_set.contains(name))
            .collect();
        let migration_results =
            plugin_migration::run_all_pending_migrations(&db, &enabled_discovered)
                .await
                .context("failed to run plugin migrations")?;
        for (plugin_name, applied) in &migration_results {
            info!(
                plugin = %plugin_name,
                count = applied.len(),
                "applied plugin migrations"
            );
        }

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
        let mut menu_registry = MenuRegistry::from_tap_results(menu_jsons);

        // Register core "Home" menu item
        menu_registry.register(crate::menu::MenuDefinition {
            path: "/".to_string(),
            title: "Home".to_string(),
            plugin: "core".to_string(),
            permission: String::new(),
            parent: None,
            weight: -10,
            visible: true,
            method: "GET".to_string(),
            handler_type: "page".to_string(),
        });

        let menu_registry = Arc::new(menu_registry);

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

        // Build Gather extension registry from plugin tap_gather_extend declarations
        let gather_extensions = {
            let mut registry = GatherExtensionRegistry::new();
            let extend_state = RequestState::without_services(UserContext::anonymous());
            let extend_results = tap_dispatcher
                .dispatch("tap_gather_extend", "{}", extend_state)
                .await;

            let mut declarations = Vec::new();
            for result in extend_results {
                match serde_json::from_str::<GatherExtensionDeclaration>(&result.output) {
                    Ok(decl) => declarations.push((result.plugin_name, decl)),
                    Err(e) => {
                        tracing::warn!(
                            plugin = %result.plugin_name,
                            error = %e,
                            "failed to parse tap_gather_extend response"
                        );
                    }
                }
            }

            let warnings = registry.apply_declarations(declarations);
            for warning in &warnings {
                tracing::warn!("{}", warning);
            }

            Arc::new(registry)
        };

        // Create gather service and load queries
        let gather = GatherService::new(db.clone(), categories.clone(), gather_extensions);
        gather
            .load_queries()
            .await
            .context("failed to load gather queries")?;

        // Create theme engine
        let template_dir = Self::resolve_template_dir();
        info!(?template_dir, "loading templates from directory");
        let theme = Arc::new(
            ThemeEngine::new(&template_dir)
                .inspect_err(
                    |e| tracing::warn!(error = ?e, "failed to load templates, using empty engine"),
                )
                .or_else(|_| ThemeEngine::empty())
                .context("failed to create theme engine")?,
        );

        // Create form service
        let forms = Arc::new(FormService::new(
            db.clone(),
            tap_dispatcher.clone(),
            theme.clone(),
        ));

        // Create cache layer (Moka L1 + Redis L2)
        let cache = CacheLayer::new(redis.clone());

        // Create search service
        let search = Arc::new(SearchService::new(db.clone()));

        // Create file service with local storage
        let file_storage = Arc::new(LocalFileStorage::new(
            &config.uploads_dir,
            &config.files_url,
        ));
        let files = Arc::new(FileService::new(db.clone(), file_storage));

        // Create cron service with file service for proper cleanup
        let cron = Arc::new(CronService::with_file_service(
            redis.clone(),
            db.clone(),
            files.clone(),
        ));

        // Create metrics
        let metrics = Arc::new(Metrics::new());

        // Create rate limiter
        let rate_limiter = Arc::new(RateLimiter::new(redis.clone(), RateLimitConfig::default()));

        // Create batch service
        let batch = Arc::new(BatchService::new(redis.clone()));

        // Create stage service
        let stage = Arc::new(StageService::new(db.clone(), cache.clone()));

        // Load languages and build negotiator chain
        let languages = crate::models::Language::list_all(&db)
            .await
            .context("failed to load languages")?;
        let known_languages: Vec<String> = languages.iter().map(|l| l.id.clone()).collect();
        let default_language = languages
            .iter()
            .find(|l| l.is_default)
            .map(|l| l.id.clone())
            .unwrap_or_else(|| "en".to_string());
        info!(
            count = known_languages.len(),
            default = %default_language,
            "loaded languages"
        );

        let mut language_negotiators: Vec<Arc<dyn LanguageNegotiator>> = vec![
            Arc::new(UrlPrefixNegotiator::new(
                known_languages.clone(),
                default_language.clone(),
            )),
            Arc::new(AcceptLanguageNegotiator::new(known_languages.clone())),
        ];
        language_negotiators.sort_by_key(|n| std::cmp::Reverse(n.priority()));

        Ok(Self {
            inner: Arc::new(AppStateInner {
                db,
                redis,
                cache,
                config_storage,
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
                search,
                theme,
                forms,
                files,
                cron,
                metrics,
                rate_limiter,
                batch,
                stage,
                language_negotiators,
                known_languages,
                default_language,
            }),
        })
    }

    /// Resolve the templates directory.
    fn resolve_template_dir() -> PathBuf {
        // Check environment variable first
        if let Ok(dir) = std::env::var("TEMPLATES_DIR") {
            return PathBuf::from(dir);
        }

        // Default to ./templates relative to working directory
        PathBuf::from("./templates")
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

    /// Get the theme engine.
    pub fn theme(&self) -> &Arc<ThemeEngine> {
        &self.inner.theme
    }

    /// Get the form service.
    pub fn forms(&self) -> &Arc<FormService> {
        &self.inner.forms
    }

    /// Get the cache layer.
    pub fn cache(&self) -> &CacheLayer {
        &self.inner.cache
    }

    /// Get the config storage.
    ///
    /// All config entity access MUST go through this interface.
    /// This is critical for future stage-aware config support.
    pub fn config_storage(&self) -> &Arc<dyn ConfigStorage> {
        &self.inner.config_storage
    }

    /// Get stage-aware config storage for a specific stage.
    ///
    /// This creates a StageAwareConfigStorage that reads/writes to the given stage,
    /// falling back to live for reads. Use this when you need to operate within
    /// a stage context.
    pub fn config_storage_for_stage(&self, stage_id: &str) -> Arc<dyn ConfigStorage> {
        if stage_id == "live" {
            // Live stage uses direct storage
            self.inner.config_storage.clone()
        } else {
            // Non-live stages use stage-aware storage
            let direct = Arc::new(DirectConfigStorage::new(self.inner.db.clone()));
            Arc::new(StageAwareConfigStorage::new(
                direct,
                self.inner.db.clone(),
                stage_id.to_string(),
            ))
        }
    }

    /// Get the search service.
    pub fn search(&self) -> &Arc<SearchService> {
        &self.inner.search
    }

    /// Get the file service.
    pub fn files(&self) -> &Arc<FileService> {
        &self.inner.files
    }

    /// Get the cron service.
    pub fn cron(&self) -> &Arc<CronService> {
        &self.inner.cron
    }

    /// Get the metrics registry.
    pub fn metrics(&self) -> &Arc<Metrics> {
        &self.inner.metrics
    }

    /// Get the rate limiter.
    pub fn rate_limiter(&self) -> &Arc<RateLimiter> {
        &self.inner.rate_limiter
    }

    /// Get the batch service.
    pub fn batch(&self) -> &Arc<BatchService> {
        &self.inner.batch
    }

    /// Get the stage service.
    pub fn stage(&self) -> &Arc<StageService> {
        &self.inner.stage
    }

    /// Get the language negotiator chain (sorted by priority descending).
    pub fn language_negotiators(&self) -> &[Arc<dyn LanguageNegotiator>] {
        &self.inner.language_negotiators
    }

    /// Get the known language codes (loaded from DB at startup).
    pub fn known_languages(&self) -> &[String] {
        &self.inner.known_languages
    }

    /// Get the default language code (loaded from DB at startup).
    pub fn default_language(&self) -> &str {
        &self.inner.default_language
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
