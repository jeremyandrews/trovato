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
use crate::services;
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

    /// Path to plugins directory on disk.
    plugins_dir: PathBuf,

    /// Set of enabled plugin names (mutable via admin UI).
    ///
    /// Uses `parking_lot::RwLock` rather than `std::sync::RwLock` because:
    /// - No poisoning: a panic in a writer won't permanently wedge every reader.
    /// - Shorter critical sections avoid blocking Tokio worker threads.
    enabled_plugins: parking_lot::RwLock<std::collections::HashSet<String>>,

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

    /// Tile rendering service.
    tiles: Arc<services::tile::TileService>,

    // --- Optional services (available when configured) ---
    /// Email delivery service (available when SMTP_HOST is configured).
    email: Option<Arc<services::email::EmailService>>,

    // --- Optional services (available when their plugins are enabled) ---
    /// Audit logging service.
    audit: Option<Arc<services::audit::AuditService>>,

    /// Content lock service.
    content_lock: Option<Arc<services::content_lock::ContentLockService>>,

    /// Image style service.
    image_styles: Option<Arc<services::image_style::ImageStyleService>>,

    /// OAuth2 service.
    oauth: Option<Arc<services::oauth::OAuthService>>,

    /// Locale service.
    locale: Option<Arc<services::locale::LocaleService>>,

    /// Redirect lookup cache (available when redirects plugin is enabled).
    redirect_cache: Option<Arc<services::redirect::RedirectCache>>,
}

impl AppState {
    /// Create new application state with database connections.
    ///
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

        // Auto-install any new plugins into plugin_status table.
        // Compute per-plugin should_enable from default_enabled and DISABLED_PLUGINS.
        let discovered_triples: Vec<(&str, &str, bool)> = discovered
            .iter()
            .map(|(name, (info, _))| {
                let should_enable = crate::plugin::gate::should_auto_enable(
                    info.default_enabled,
                    &config.disabled_plugins,
                    name,
                );
                (name.as_str(), info.version.as_str(), should_enable)
            })
            .collect();
        let new_count = plugin_status::auto_install_new_plugins(&db, &discovered_triples)
            .await
            .context("failed to auto-install new plugins")?;
        if new_count > 0 {
            info!(count = new_count, "auto-installed new plugins");
        }

        // Warn per-entry about DISABLED_PLUGINS that were already installed
        // (env var only affects first-time installs via ON CONFLICT DO NOTHING).
        if !config.disabled_plugins.is_empty() {
            let statuses = plugin_status::get_all_statuses(&db)
                .await
                .unwrap_or_default();
            let installed: std::collections::HashSet<&str> =
                statuses.iter().map(|s| s.name.as_str()).collect();
            let stale: Vec<&str> = config
                .disabled_plugins
                .iter()
                .filter(|p| installed.contains(p.as_str()))
                .map(|s| s.as_str())
                .collect();
            if !stale.is_empty() {
                info!(
                    plugins = ?stale,
                    "DISABLED_PLUGINS entries are already installed; \
                     the env var only affects first-time installs. \
                     Use the admin UI or CLI to change plugin status."
                );
            }
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

        // Register core default gather queries (only adds if not already in DB)
        gather
            .register_default_views()
            .await
            .context("failed to register default gather queries")?;

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
        let mut cron = CronService::with_file_service(redis.clone(), db.clone(), files.clone());

        // Create metrics
        let metrics = Arc::new(Metrics::new());

        // Create rate limiter
        let rate_limiter = Arc::new(RateLimiter::new(redis.clone(), RateLimitConfig::default()));

        // Create batch service
        let batch = Arc::new(BatchService::new(redis.clone()));

        // Create stage service
        let stage = Arc::new(StageService::new(db.clone(), cache.clone()));

        // Create tile service
        let tiles = Arc::new(services::tile::TileService::new(db.clone()));

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

        // Initialize email service (conditionally, when SMTP_HOST is set)
        let email = config.smtp_host.as_ref().and_then(|host| {
            match services::email::EmailService::new(
                host,
                config.smtp_port,
                config.smtp_username.as_deref(),
                config.smtp_password.as_deref(),
                &config.smtp_encryption,
                config.smtp_from_email.clone(),
                config.site_url.clone(),
            ) {
                Ok(svc) => {
                    info!(host = %host, port = config.smtp_port, "SMTP email service configured");
                    Some(Arc::new(svc))
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to initialize email service");
                    None
                }
            }
        });

        // Initialize optional services based on enabled plugins
        let audit = if enabled_set.contains("audit_log") {
            Some(Arc::new(services::audit::AuditService::new(db.clone())))
        } else {
            None
        };

        let content_lock = if enabled_set.contains("content_locking") {
            Some(Arc::new(services::content_lock::ContentLockService::new(
                db.clone(),
            )))
        } else {
            None
        };

        let image_styles = if enabled_set.contains("image_styles") {
            Some(Arc::new(services::image_style::ImageStyleService::new(
                db.clone(),
                std::path::Path::new(&config.uploads_dir),
            )))
        } else {
            None
        };

        let oauth = if enabled_set.contains("oauth2") {
            match std::env::var("JWT_SECRET") {
                Ok(secret) if secret.len() >= 32 => {
                    // Warn about low-entropy secrets
                    let unique_chars: std::collections::HashSet<u8> = secret.bytes().collect();
                    if unique_chars.len() < 8 {
                        tracing::warn!(
                            unique_chars = unique_chars.len(),
                            "JWT_SECRET has low character diversity; consider using a more random value"
                        );
                    }
                    Some(Arc::new(services::oauth::OAuthService::new(
                        db.clone(),
                        secret.as_bytes(),
                    )))
                }
                Ok(secret) => {
                    tracing::error!(
                        len = secret.len(),
                        "JWT_SECRET is too short (must be >= 32 bytes); OAuth2 disabled"
                    );
                    None
                }
                Err(_) => {
                    tracing::error!("JWT_SECRET environment variable is not set; OAuth2 disabled");
                    None
                }
            }
        } else {
            None
        };

        let locale = if enabled_set.contains("locale") {
            let locale_service = services::locale::LocaleService::new(db.clone());
            // Pre-load translations for default language
            if let Err(e) = locale_service.load_language(&default_language).await {
                tracing::warn!(error = %e, "failed to pre-load locale translations");
            }
            Some(Arc::new(locale_service))
        } else {
            None
        };

        // Wire plugin services into cron
        cron.set_plugin_services(content_lock.clone(), audit.clone());
        cron.set_tap_dispatcher(tap_dispatcher.clone());
        let cron = Arc::new(cron);

        Ok(Self {
            inner: Arc::new(AppStateInner {
                db,
                plugins_dir: config.plugins_dir.clone(),
                enabled_plugins: parking_lot::RwLock::new(enabled_set.clone()),
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
                tiles,
                email,
                audit,
                content_lock,
                image_styles,
                oauth,
                locale,
                redirect_cache: if enabled_set.contains("redirects") {
                    Some(Arc::new(services::redirect::RedirectCache::new()))
                } else {
                    None
                },
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

    /// Get the plugins directory path.
    pub fn plugins_dir(&self) -> &std::path::Path {
        &self.inner.plugins_dir
    }

    /// Check if a plugin is enabled at runtime.
    pub fn is_plugin_enabled(&self, plugin: &str) -> bool {
        self.inner.enabled_plugins.read().contains(plugin)
    }

    /// Get a snapshot of the enabled plugin names.
    pub fn enabled_plugins(&self) -> std::collections::HashSet<String> {
        self.inner.enabled_plugins.read().clone()
    }

    /// Update the in-memory enabled state for a plugin.
    pub fn set_plugin_enabled(&self, plugin: &str, enabled: bool) {
        let mut set = self.inner.enabled_plugins.write();
        if enabled {
            set.insert(plugin.to_string());
        } else {
            set.remove(plugin);
        }
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

    /// Get the tile service.
    pub fn tiles(&self) -> &Arc<services::tile::TileService> {
        &self.inner.tiles
    }

    /// Get the email service (if SMTP is configured).
    pub fn email(&self) -> Option<&Arc<services::email::EmailService>> {
        self.inner.email.as_ref()
    }

    /// Get the audit service (if audit_log plugin is enabled).
    pub fn audit(&self) -> Option<&Arc<services::audit::AuditService>> {
        self.inner.audit.as_ref()
    }

    /// Get the content lock service (if content_locking plugin is enabled).
    pub fn content_lock(&self) -> Option<&Arc<services::content_lock::ContentLockService>> {
        self.inner.content_lock.as_ref()
    }

    /// Get the image style service (if image_styles plugin is enabled).
    pub fn image_styles(&self) -> Option<&Arc<services::image_style::ImageStyleService>> {
        self.inner.image_styles.as_ref()
    }

    /// Get the OAuth2 service (if oauth2 plugin is enabled).
    pub fn oauth(&self) -> Option<&Arc<services::oauth::OAuthService>> {
        self.inner.oauth.as_ref()
    }

    /// Get the locale service (if locale plugin is enabled).
    pub fn locale(&self) -> Option<&Arc<services::locale::LocaleService>> {
        self.inner.locale.as_ref()
    }

    /// Get the redirect cache (if redirects plugin is enabled).
    pub fn redirect_cache(&self) -> Option<&Arc<services::redirect::RedirectCache>> {
        self.inner.redirect_cache.as_ref()
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
