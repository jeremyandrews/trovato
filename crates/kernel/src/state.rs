//! Application state shared across all handlers.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use redis::Client as RedisClient;
use sqlx::PgPool;

use tracing::{error, info, warn};

use crate::middleware::language::{
    AcceptLanguageNegotiator, LanguageNegotiator, UrlPrefixNegotiator,
};

use crate::assistant::AssistantRegistry;
use crate::batch::BatchService;
use crate::cache::CacheLayer;
use crate::config::{CacheConfig, Config};
use crate::config_storage::{ConfigStorage, DirectConfigStorage, StageAwareConfigStorage};
use crate::content::{ContentTypeRegistry, ItemService, RecordTypeRegistry};
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
use crate::tap::{RequestServices, TapDispatcher, TapRegistry};
use crate::theme::ThemeEngine;

/// Shared application state.
///
/// Wrapped in Arc internally so Clone is cheap.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

struct AppStateInner {
    /// The configuration request handling reads.
    ///
    /// Every value here was previously read from the process environment at the
    /// point of use — per request, per response, or per query. Resolved once by
    /// `Config::from_env` and carried here, so handlers and middleware take an
    /// input instead of consulting a process-global.
    runtime: crate::config::RuntimeConfig,

    /// PostgreSQL connection pool.
    db: PgPool,

    /// Maximum database connections configured for the pool.
    db_pool_max_connections: u32,

    /// Plugin search path on disk.
    plugins_dirs: Vec<PathBuf>,

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

    /// Shared services template for tap dispatch — cloned per invocation.
    tap_services: RequestServices,

    /// Menu registry.
    menu_registry: Arc<MenuRegistry>,

    /// Assistant scope registry, built once at boot from `tap_assistant_scopes`.
    assistant_scopes: Arc<AssistantRegistry>,

    /// Content type registry.
    content_types: Arc<ContentTypeRegistry>,

    /// Lightweight-record type registry (P11g / D-53, D-54). Empty when no
    /// plugin declares `[[record_types]]`.
    record_types: Arc<RecordTypeRegistry>,

    /// Item service.
    items: Arc<ItemService>,

    /// Category service.
    categories: Arc<CategoryService>,

    /// Gather service.
    gather: Arc<GatherService>,

    /// RSS feed autodiscovery links, from the gather queries that declare a
    /// feed.
    ///
    /// Resolved once at startup rather than per render, for two reasons. The
    /// feed *routes* are registered once from the same query set, so a list
    /// rebuilt per request could advertise a feed that no route serves until the
    /// next restart. And the query cache hands out cloned definitions, so
    /// rebuilding this per request would deep-clone every gather query to read
    /// two strings out of a few of them.
    feed_links: Vec<crate::routes::feed::FeedLink>,

    /// Search service for full-text search.
    search: Arc<SearchService>,

    /// AI provider registry for managing LLM configurations.
    ai_providers: Arc<services::ai_provider::AiProviderService>,

    /// AI token budget service for usage tracking and enforcement.
    ai_budgets: Arc<services::ai_token_budget::AiTokenBudgetService>,

    /// AI chat service for streaming chatbot.
    ai_chat: Arc<services::ai_chat::ChatService>,

    /// AI Assistant configuration service.
    ai_assistant: Arc<services::ai_assistant::AssistantService>,

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

    /// User service for user CRUD with tap integration and caching.
    users: Arc<services::user::UserService>,

    /// Role service for role/permission management with cache invalidation.
    roles: Arc<services::role::RoleService>,

    /// Tile rendering service.
    tiles: Arc<services::tile::TileService>,

    /// The kernel-internal append-only security audit stream (Epic 4).
    ///
    /// Non-optional kernel infrastructure, distinct from the plugin-gated
    /// `audit` field below: authentication, credential, session, and recovery
    /// events all land here so incident response has one place to look.
    security_audit: Arc<crate::audit::SecurityAudit>,

    /// The WebAuthn relying party (FR-7a / D-34).
    ///
    /// Non-optional: auth is core infrastructure, and a WASM plugin can neither
    /// hold ceremony state nor speak the browser WebAuthn API.
    webauthn: Arc<webauthn_rs::Webauthn>,

    /// The per-user session index backing multi-device session management
    /// (FR-7b / D-36). Extends tower-sessions; does not fork it.
    session_registry: Arc<services::session_registry::SessionRegistry>,

    /// Kernel-owned account-recovery flow storage (FR-7c / D-38): the nonce,
    /// its TTL, and its single-use property. A plugin never sees this.
    recovery_flows: Arc<services::recovery_flow::RecoveryFlowStore>,

    /// The built-in recovery providers (FR-7c AC-2). They speak the frozen
    /// `tap_account_recovery` contract and are folded by the same owner-scoped
    /// fail-closed fold as any plugin's answer.
    recovery_providers: Vec<Arc<dyn services::recovery_flow::RecoveryProvider>>,

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

    /// Comment service (available when comments plugin is enabled).
    ///
    /// Uses `OnceLock` rather than `Option` so the service can be
    /// late-initialized when the plugin is enabled after `AppState`
    /// construction (e.g. in test helpers via `init_comments_service()`).
    comments: OnceLock<Arc<services::comment::CommentService>>,
}

impl AppState {
    /// Create new application state with database connections.
    ///
    pub async fn new(config: &Config) -> Result<Self> {
        // Load cache configuration from environment
        let cache_config = CacheConfig::from_env();

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
        let permissions = PermissionService::new(db.clone(), cache_config.ttl_permissions);

        // Create lockout service
        let lockout = LockoutService::new(redis.clone());

        // Discover plugins on disk (parse info.toml without compiling WASM)
        let discovered = PluginRuntime::discover_plugins(&config.plugins_dirs).await;

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

        // Create plugin runtime and load only enabled plugins. Resource bounds
        // (pool sizing + per-Store limiter + optional fuel, WASM-4) come from the
        // environment, falling back to the documented defaults.
        let plugin_config = PluginConfig::from_env();
        let mut plugin_runtime =
            PluginRuntime::new(&plugin_config).context("failed to create plugin runtime")?;

        plugin_runtime
            .load_enabled(&config.plugins_dirs, &enabled_set)
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

        use crate::tap::{RequestServices, RequestState, UserContext};

        // Dispatch tap_install for enabled plugins that haven't had it called yet.
        // This covers both auto-installed plugins (first server start after adding
        // a plugin with default_enabled=true) and CLI-installed plugins (first
        // server start after `trovato plugin install <name>`).
        //
        // Runs in a background task so the HTTP server starts immediately rather
        // than blocking for up to 150 s per plugin.
        let pending = plugin_status::get_pending_tap_install(&db)
            .await
            .context("failed to query pending tap_install")?;
        if !pending.is_empty() {
            let tap_install_db = db.clone();
            let tap_install_dispatcher = tap_dispatcher.clone();
            tokio::spawn(async move {
                let http = crate::host::http::build_outbound_client();
                for plugin_name in &pending {
                    let install_state = RequestState::new(
                        UserContext::anonymous(),
                        RequestServices::for_background(
                            tap_install_db.clone(),
                            None,
                            None,
                            http.clone(),
                        )
                        .with_plugin_runtime(tap_install_dispatcher.runtime().clone()),
                    );
                    // dispatch_to_plugin returns None when the plugin does not
                    // export tap_install (harmless) OR when the WASM call fails
                    // (the dispatcher already logs an error in that case).
                    // Mark called in either case — to retry, set
                    // tap_install_called = FALSE in plugin_status and restart.
                    let result = tap_install_dispatcher
                        .dispatch_to_plugin("tap_install", "{}", plugin_name, install_state)
                        .await;
                    if result.is_some() {
                        info!(plugin = %plugin_name, "tap_install dispatched");
                    } else {
                        warn!(
                            plugin = %plugin_name,
                            "tap_install not implemented or failed — check error log; \
                             reset tap_install_called = FALSE in plugin_status to retry"
                        );
                    }
                    if let Err(e) =
                        plugin_status::mark_tap_install_called(&tap_install_db, plugin_name).await
                    {
                        error!(
                            plugin = %plugin_name,
                            error = %e,
                            "failed to mark tap_install_called"
                        );
                    }
                }
            });
        }

        // Create menu registry from plugins by invoking tap_menu
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
            callback: String::new(),
            parent: None,
            weight: -10,
            visible: true,
            method: "GET".to_string(),
            handler_type: "page".to_string(),
            local_task: false,
        });

        let menu_registry = Arc::new(menu_registry);

        // Assistant scopes, collected the same way and for the same reason: a
        // scope names a route the kernel has to serve and a permission it has to
        // check, so it must be known before the first request. Dispatched
        // without services, like `tap_menu` — a scope declaration is a constant,
        // not a query.
        let assistant_scopes = {
            let scope_state = RequestState::without_services(UserContext::anonymous());
            let results = tap_dispatcher
                .dispatch("tap_assistant_scopes", "{}", scope_state)
                .await;
            let registry = AssistantRegistry::from_tap_results(
                results
                    .into_iter()
                    .map(|r| (r.plugin_name, r.output))
                    .collect(),
            );
            if !registry.is_empty() {
                info!(count = registry.len(), "registered assistant scopes");
            }
            Arc::new(registry)
        };

        // Create content type registry
        let content_types = Arc::new(ContentTypeRegistry::new(
            db.clone(),
            cache_config.ttl_content_types,
        ));
        content_types
            .sync_from_plugins(&tap_dispatcher)
            .await
            .context("failed to sync content types")?;

        // Build the lightweight-record type registry (P11g / D-53, D-54) from
        // every loaded plugin's `[[record_types]]` declarations. Validated here
        // against each plugin's effective DB allowlist and against the content-type
        // names, since both are now known. Rejected declarations are logged, not
        // fatal — one plugin's bad record declaration must not abort boot.
        let record_types = {
            let content_type_names: std::collections::HashSet<String> = content_types
                .list_all()
                .await
                .into_iter()
                .map(|ct| ct.machine_name)
                .collect();
            let sources: Vec<(
                &str,
                &crate::plugin::DbPolicy,
                &[crate::plugin::RecordTypeDecl],
            )> = plugin_runtime
                .plugins()
                .iter()
                .map(|(name, plugin)| {
                    (
                        name.as_str(),
                        plugin.db_policy().as_ref(),
                        plugin.info.record_types.as_slice(),
                    )
                })
                .collect();
            let (registry, errors) =
                crate::content::RecordTypeRegistry::build(sources, &content_type_names);
            for err in &errors {
                warn!(error = %err, "skipping invalid lightweight-record declaration");
            }
            if !registry.is_empty() {
                info!(
                    count = registry.len(),
                    "registered lightweight-record types"
                );
            }
            Arc::new(registry)
        };

        // Create category service
        let categories = CategoryService::new(db.clone(), cache_config.ttl_categories);

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

        // Create AI provider service (also backs gather semantic search and
        // item-index embedding generation, so it is built before both).
        let ai_providers = Arc::new(services::ai_provider::AiProviderService::new(db.clone()));

        // Create the pgvector store. Always constructable; it self-reports
        // availability (false when the extension is absent) and degrades
        // gracefully. Shared by gather (read) and item index (write) so both
        // sides see the same embeddings.
        let vector_store = services::vector_store::create_default_vector_store(db.clone()).await;

        // Create gather service and load queries
        let gather = GatherService::new(
            db.clone(),
            categories.clone(),
            gather_extensions,
            crate::gather::GatherConfig {
                ttl: cache_config.ttl_gather_queries,
                max_page_size: config.gather_max_page_size,
                access: config.runtime.gather_access,
            },
            Some(ai_providers.clone()),
            Some(vector_store.clone()),
        );
        gather
            .load_queries()
            .await
            .context("failed to load gather queries")?;

        // Register core default gather queries (only adds if not already in DB)
        gather
            .register_default_views()
            .await
            .context("failed to register default gather queries")?;

        // Feed autodiscovery links, from the same query set `main` builds the
        // feed router from.
        let feed_links = crate::routes::feed::feed_links(&gather.list_queries());

        // Load languages early so locale service can pre-load translations
        // and the trans filter is available when theme engine is created.
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

        // Initialize locale service (before theme engine so trans filter is available)
        let locale = if enabled_set.contains("trovato_locale") {
            let locale_service = services::locale::LocaleService::new(db.clone());
            if let Err(e) = locale_service.load_language(&default_language).await {
                tracing::warn!(error = %e, "failed to pre-load locale translations");
            }
            Some(Arc::new(locale_service))
        } else {
            None
        };

        // Create theme engine. The search path is a `Config` field, resolved at
        // startup, so an embedder points the engine at its own templates by
        // setting a field rather than exporting `TEMPLATES_DIR`.
        info!(template_dirs = ?config.templates_dirs, "loading templates from search path");
        let theme = Arc::new(
            ThemeEngine::new(&config.templates_dirs, locale.clone())
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

        // (ai_providers and vector_store were created earlier, before the
        // gather service, since gather semantic search depends on them.)

        // Create AI token budget service
        let ai_budgets = Arc::new(services::ai_token_budget::AiTokenBudgetService::new(
            db.clone(),
        ));

        // Create AI chat service
        let ai_chat = Arc::new(services::ai_chat::ChatService::new(
            db.clone(),
            ai_providers.clone(),
            search.clone(),
        ));

        // The AI Assistant's configuration. The turn loop itself takes
        // `&AppState`, because it needs the tap dispatcher, the scope registry,
        // the provider service and the budget service all at once.
        let ai_assistant = Arc::new(services::ai_assistant::AssistantService::new(db.clone()));

        // Build shared RequestServices for tap dispatch.
        // This gives plugins access to DB, cache, AI, and HTTP from host functions.
        let tap_http = crate::host::http::build_outbound_client();
        // One shared field-access decision cache (design amendment α). Set on the
        // tap_services template so ItemService (built from it below) and every
        // production tap dispatch — including the `variables::set` flush — operate
        // on the same instance.
        let field_access_cache = std::sync::Arc::new(crate::tap::new_field_access_cache());

        // Initialize email service (conditionally, when SMTP_HOST is set).
        //
        // Built here, before the tap services template, because the `mail` host
        // interface is served from that template: a plugin sending mail uses this
        // same service, and therefore the same SMTP transport, `from` address and
        // circuit breaker the kernel's own mail uses.
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

        let mut tap_services = RequestServices::for_request(
            db.clone(),
            Some(cache.clone()),
            Some(ai_providers.clone()),
            Some(ai_budgets.clone()),
            tap_http,
        )
        // Attach the runtime so plugin host functions can invoke other plugins
        // (FR-4a). Cloned into item/user/comment services below, so they inherit it.
        .with_plugin_runtime(plugin_runtime.clone())
        .with_field_access_cache(field_access_cache);
        if let Some(ref email) = email {
            tap_services = tap_services.with_email(email.clone());
        }
        let tap_services = tap_services;

        // Create item service (needs tap_services for presave/insert/update taps;
        // ai_providers + vector_store drive kernel embedding regeneration on
        // tap_item_update_index).
        let items = Arc::new(ItemService::new(
            db.clone(),
            tap_dispatcher.clone(),
            tap_services.clone(),
            cache_config.ttl_items,
            Some(ai_providers.clone()),
            Some(vector_store.clone()),
        ));

        // Late-bind the item-access seam into the gather service (Story 3.4):
        // gather is constructed before ItemService, so it holds a OnceLock the
        // item service fills here. Every gather read path now runs the shared
        // check_access + field-access seam over its result page.
        gather.set_item_service(items.clone());

        // Late-bind the lightweight-record registry into gather (P11g / D-54), so
        // a gather naming a `record_type` resolves it to its table + field map.
        gather.set_record_types(record_types.clone());

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

        // Create rate limiter. Trusted proxies (whose X-Forwarded-For is
        // believed) are parsed once by `Config::from_env` from TRUSTED_PROXIES;
        // an empty list ⇒ trust none (RATE-1).
        let rate_limiter = Arc::new(RateLimiter::new(
            redis.clone(),
            RateLimitConfig::default(),
            config.trusted_proxies.clone(),
        ));

        // Create batch service
        let batch = Arc::new(BatchService::new(redis.clone()));

        // Create stage service
        let stage = Arc::new(StageService::new(db.clone(), cache.clone()));

        // Create user service
        let users = Arc::new(services::user::UserService::new(
            db.clone(),
            tap_dispatcher.clone(),
            tap_services.clone(),
            cache_config.ttl_users,
        ));

        // Create role service (depends on permission service for cache invalidation)
        let roles = Arc::new(services::role::RoleService::new(
            db.clone(),
            permissions.clone(),
        ));

        // Create tile service
        let tiles = Arc::new(services::tile::TileService::new(db.clone()));

        // Build language negotiator chain (languages were loaded earlier for locale)
        let mut language_negotiators: Vec<Arc<dyn LanguageNegotiator>> = vec![
            Arc::new(UrlPrefixNegotiator::new(
                known_languages.clone(),
                default_language.clone(),
            )),
            Arc::new(AcceptLanguageNegotiator::new(known_languages.clone())),
        ];
        language_negotiators.sort_by_key(|n| std::cmp::Reverse(n.priority()));

        // The kernel-internal security audit stream. Always present: an audit
        // trail that is only sometimes written is not an audit trail.
        let security_audit = Arc::new(crate::audit::SecurityAudit::new(db.clone()));

        // The WebAuthn relying party, derived from SITE_URL so the RP ID and the
        // origin can never drift apart (a mismatch silently invalidates every
        // registered credential).
        let site_name = crate::models::SiteConfig::site_name(&db)
            .await
            .unwrap_or_else(|_| "Trovato".to_string());
        let webauthn = Arc::new(
            services::webauthn::build_webauthn(&config.site_url, &site_name)
                .context("failed to initialize the WebAuthn relying party")?,
        );

        // The per-user session index. Its TTL matches the session's own
        // inactivity expiry so the registry and the sessions it describes
        // cannot disagree about what is still alive.
        let session_registry = Arc::new(services::session_registry::SessionRegistry::new(
            redis.clone(),
            crate::session::DEFAULT_SESSION_EXPIRY_HOURS * 3600,
        ));

        // Kernel-owned recovery flow storage and the two built-in paths.
        let recovery_flows = Arc::new(services::recovery_flow::RecoveryFlowStore::new(
            redis.clone(),
        ));
        let recovery_providers: Vec<Arc<dyn services::recovery_flow::RecoveryProvider>> = vec![
            Arc::new(services::recovery_builtins::EmailRecoveryProvider::new(
                db.clone(),
                email.clone(),
                site_name.clone(),
            )),
            Arc::new(services::recovery_builtins::RecoveryCodesProvider::new(
                db.clone(),
            )),
        ];

        // Initialize optional services based on enabled plugins
        let audit = if enabled_set.contains("trovato_audit_log") {
            Some(Arc::new(services::audit::AuditService::new(db.clone())))
        } else {
            None
        };

        let content_lock = if enabled_set.contains("trovato_content_locking") {
            Some(Arc::new(services::content_lock::ContentLockService::new(
                db.clone(),
            )))
        } else {
            None
        };

        let image_styles = if enabled_set.contains("trovato_image_styles") {
            Some(Arc::new(services::image_style::ImageStyleService::new(
                db.clone(),
                std::path::Path::new(&config.uploads_dir),
            )))
        } else {
            None
        };

        let oauth = if enabled_set.contains("trovato_oauth2") {
            match config.jwt_secret.as_deref() {
                Some(secret) if secret.len() >= 32 => {
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
                Some(secret) => {
                    tracing::error!(
                        len = secret.len(),
                        "JWT_SECRET is too short (must be >= 32 bytes); OAuth2 disabled"
                    );
                    None
                }
                None => {
                    tracing::error!("JWT_SECRET is not configured; OAuth2 disabled");
                    None
                }
            }
        } else {
            None
        };

        let comments = OnceLock::new();
        if enabled_set.contains("trovato_comments") {
            let _ = comments.set(Arc::new(services::comment::CommentService::new(
                db.clone(),
                tap_dispatcher.clone(),
                tap_services.clone(),
            )));
        }

        // Wire plugin services into cron
        cron.set_plugin_services(content_lock.clone(), audit.clone());
        cron.set_email_service(email.clone());
        cron.set_tap_dispatcher(tap_dispatcher.clone());
        cron.set_ai_providers(ai_providers.clone());
        cron.set_ai_budgets(ai_budgets.clone());
        // P11f: the native embed drain writes item embeddings into the same
        // pgvector store the gather read path and sync index path share.
        cron.set_vector_store(vector_store.clone());
        cron.set_pagefind_enabled(enabled_set.contains("trovato_search"));
        // The security-audit retention window and the Pagefind index destination
        // used to be read from the environment inside the tasks themselves.
        cron.apply_runtime_config(&config.runtime);
        let cron = Arc::new(cron);

        // Spawn background cache reload tasks for collection caches.
        // These periodically reload all entries from the database so that
        // external changes (CLI config import, second server) become visible
        // within the TTL window.
        {
            let ct = content_types.clone();
            let interval = cache_config.ttl_content_types;
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(interval);
                tick.tick().await; // skip immediate first tick
                loop {
                    tick.tick().await;
                    if let Err(e) = ct.reload_from_db().await {
                        warn!(error = %e, "failed to reload content types from database");
                    }
                }
            });
        }
        {
            let gq = gather.clone();
            let interval = cache_config.ttl_gather_queries;
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(interval);
                tick.tick().await; // skip immediate first tick
                loop {
                    tick.tick().await;
                    if let Err(e) = gq.reload_from_db().await {
                        warn!(error = %e, "failed to reload gather queries from database");
                    }
                }
            });
        }

        Ok(Self {
            inner: Arc::new(AppStateInner {
                runtime: config.runtime.clone(),
                db,
                db_pool_max_connections: config.database_max_connections,
                plugins_dirs: config.plugins_dirs.clone(),
                enabled_plugins: parking_lot::RwLock::new(enabled_set.clone()),
                redis,
                cache,
                config_storage,
                permissions,
                lockout,
                plugin_runtime,
                tap_registry,
                tap_dispatcher,
                tap_services,
                menu_registry,
                assistant_scopes,
                content_types,
                record_types,
                items,
                categories,
                gather,
                feed_links,
                search,
                ai_providers,
                ai_budgets,
                ai_chat,
                ai_assistant,
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
                users,
                roles,
                tiles,
                security_audit,
                webauthn,
                session_registry,
                recovery_flows,
                recovery_providers,
                email,
                audit,
                content_lock,
                image_styles,
                oauth,
                locale,
                redirect_cache: if enabled_set.contains("trovato_redirects") {
                    Some(Arc::new(services::redirect::RedirectCache::new()))
                } else {
                    None
                },
                comments,
            }),
        })
    }

    /// The configuration request handling reads.
    ///
    /// One accessor rather than one per value: these travel together as
    /// configuration, and grouping them keeps the reason they exist legible —
    /// each was an environment read at the point of use before.
    pub fn runtime(&self) -> &crate::config::RuntimeConfig {
        &self.inner.runtime
    }

    /// Get the database pool.
    pub fn db(&self) -> &PgPool {
        &self.inner.db
    }

    /// Get the configured maximum database pool connections.
    pub fn db_pool_max_connections(&self) -> u32 {
        self.inner.db_pool_max_connections
    }

    /// Get the plugin search path.
    pub fn plugins_dirs(&self) -> &[PathBuf] {
        &self.inner.plugins_dirs
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
    ///
    /// When enabling `"comments"`, this also late-initializes the
    /// `CommentService` if it wasn't created at startup.
    pub fn set_plugin_enabled(&self, plugin: &str, enabled: bool) {
        let mut set = self.inner.enabled_plugins.write();
        if enabled {
            set.insert(plugin.to_string());
            // Late-init plugin services that weren't created at startup.
            if plugin == "trovato_comments" {
                self.init_comments_service();
            }
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

    /// Shared services template for tap dispatch.
    pub fn tap_services(&self) -> &RequestServices {
        &self.inner.tap_services
    }

    /// Get the menu registry.
    pub fn menu_registry(&self) -> &Arc<MenuRegistry> {
        &self.inner.menu_registry
    }

    /// Get the assistant scope registry.
    pub fn assistant_scopes(&self) -> &Arc<AssistantRegistry> {
        &self.inner.assistant_scopes
    }

    /// Get the content type registry.
    pub fn content_types(&self) -> &Arc<ContentTypeRegistry> {
        &self.inner.content_types
    }

    /// Get the lightweight-record type registry (P11g / D-53, D-54).
    pub fn record_types(&self) -> &Arc<RecordTypeRegistry> {
        &self.inner.record_types
    }

    /// Get the item service.
    pub fn items(&self) -> &Arc<ItemService> {
        &self.inner.items
    }

    /// Get the category service.
    pub fn categories(&self) -> &Arc<CategoryService> {
        &self.inner.categories
    }

    /// RSS feed autodiscovery links for the site's head.
    ///
    /// Frozen at startup, so it always matches the registered feed routes.
    pub fn feed_links(&self) -> &[crate::routes::feed::FeedLink] {
        &self.inner.feed_links
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
    pub fn config_storage_for_stage(&self, stage_id: uuid::Uuid) -> Arc<dyn ConfigStorage> {
        if stage_id == crate::models::stage::LIVE_STAGE_ID {
            // Live stage uses direct storage
            self.inner.config_storage.clone()
        } else {
            // Non-live stages use stage-aware storage
            let direct = Arc::new(DirectConfigStorage::new(self.inner.db.clone()));
            Arc::new(StageAwareConfigStorage::new(
                direct,
                self.inner.db.clone(),
                stage_id,
            ))
        }
    }

    /// Get the search service.
    pub fn search(&self) -> &Arc<SearchService> {
        &self.inner.search
    }

    /// Get the AI provider service.
    pub fn ai_providers(&self) -> &Arc<services::ai_provider::AiProviderService> {
        &self.inner.ai_providers
    }

    /// Get the AI token budget service.
    pub fn ai_budgets(&self) -> &Arc<services::ai_token_budget::AiTokenBudgetService> {
        &self.inner.ai_budgets
    }

    /// Get the AI chat service.
    pub fn ai_chat(&self) -> &Arc<services::ai_chat::ChatService> {
        &self.inner.ai_chat
    }

    /// Get the AI Assistant configuration service.
    pub fn ai_assistant(&self) -> &Arc<services::ai_assistant::AssistantService> {
        &self.inner.ai_assistant
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

    /// Get the user service.
    pub fn users(&self) -> &Arc<services::user::UserService> {
        &self.inner.users
    }

    /// Get the role service.
    pub fn roles(&self) -> &Arc<services::role::RoleService> {
        &self.inner.roles
    }

    /// Get the tile service.
    pub fn tiles(&self) -> &Arc<services::tile::TileService> {
        &self.inner.tiles
    }

    /// Get the email service (if SMTP is configured).
    pub fn email(&self) -> Option<&Arc<services::email::EmailService>> {
        self.inner.email.as_ref()
    }

    /// The kernel-internal append-only security audit stream (Epic 4).
    ///
    /// Always available. This is where authentication, credential, session, and
    /// recovery events go — not [`AppState::audit`], which is the optional
    /// `trovato_audit_log` plugin's content-CRUD log.
    pub fn security_audit(&self) -> &Arc<crate::audit::SecurityAudit> {
        &self.inner.security_audit
    }

    /// The WebAuthn relying party (FR-7a / D-34).
    pub fn webauthn(&self) -> &Arc<webauthn_rs::Webauthn> {
        &self.inner.webauthn
    }

    /// The per-user session index (FR-7b / D-36).
    pub fn session_registry(&self) -> &Arc<services::session_registry::SessionRegistry> {
        &self.inner.session_registry
    }

    /// Kernel-owned account-recovery flow storage (FR-7c).
    pub fn recovery_flows(&self) -> &Arc<services::recovery_flow::RecoveryFlowStore> {
        &self.inner.recovery_flows
    }

    /// The built-in recovery providers (FR-7c AC-2).
    pub fn recovery_providers(&self) -> &[Arc<dyn services::recovery_flow::RecoveryProvider>] {
        &self.inner.recovery_providers
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

    /// Get the comment service (if comments plugin is enabled).
    ///
    /// # Panics
    ///
    /// Callers behind the `plugin_gate!(gate_comments, "trovato_comments")` guard can
    /// safely unwrap because the gate ensures the plugin is enabled and the
    /// service is initialized.
    #[allow(clippy::expect_used)] // Callers are behind plugin_gate! — see doc above.
    pub fn comments(&self) -> &Arc<services::comment::CommentService> {
        self.inner
            .comments
            .get()
            .expect("comments service not initialized — caller must be behind plugin gate")
    }

    /// The comments service, or `None` when comments are unavailable.
    ///
    /// [`Self::comments`] panics off the plugin gate, which is right for the
    /// comment routes but wrong for a page render: an item page must not 500
    /// because comments are switched off. Checks both conditions — the plugin
    /// enabled *and* the service initialized — since the service is
    /// late-initialized when the plugin is enabled after startup.
    pub fn comments_if_enabled(&self) -> Option<&Arc<services::comment::CommentService>> {
        if !self.is_plugin_enabled("trovato_comments") {
            return None;
        }
        self.inner.comments.get()
    }

    /// Late-initialize the comments service.
    ///
    /// Called by `set_plugin_enabled("trovato_comments", true)` when the service was
    /// not created at `AppState` construction time (e.g. in tests where
    /// plugins are enabled after startup).  No-op if already initialized.
    pub fn init_comments_service(&self) {
        let _ = self
            .inner
            .comments
            .set(Arc::new(services::comment::CommentService::new(
                self.inner.db.clone(),
                self.inner.tap_dispatcher.clone(),
                self.inner.tap_services.clone(),
            )));
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

    /// Build a structured health report for load balancers and monitoring.
    pub async fn health_report(&self) -> HealthReport {
        let (pg_ok, redis_ok) = tokio::join!(self.postgres_healthy(), self.redis_healthy());

        let db = if pg_ok {
            let size = self.inner.db.size();
            let idle = self.inner.db.num_idle() as u32;
            let active = size.saturating_sub(idle);
            let max = self.inner.db_pool_max_connections;
            let pct = (active * 100).checked_div(max).unwrap_or(0);
            if pct >= 80 {
                ServiceHealth::Degraded(format!(
                    "pool utilization at {pct}% ({active}/{max} connections active)"
                ))
            } else {
                ServiceHealth::Healthy
            }
        } else {
            ServiceHealth::Unavailable("PostgreSQL is not responding".to_string())
        };

        let redis = if redis_ok {
            ServiceHealth::Healthy
        } else {
            ServiceHealth::Unavailable("Redis is not responding".to_string())
        };

        let plugin_count = self.inner.plugin_runtime.plugin_count();
        let plugins = if plugin_count > 0 {
            ServiceHealth::Healthy
        } else {
            ServiceHealth::Degraded("no plugins loaded".to_string())
        };

        let optional = vec![
            ("email".to_string(), opt_health(&self.inner.email)),
            ("audit".to_string(), opt_health(&self.inner.audit)),
            (
                "content_lock".to_string(),
                opt_health(&self.inner.content_lock),
            ),
            (
                "image_styles".to_string(),
                opt_health(&self.inner.image_styles),
            ),
            ("oauth".to_string(), opt_health(&self.inner.oauth)),
            ("locale".to_string(), opt_health(&self.inner.locale)),
            (
                "redirects".to_string(),
                opt_health(&self.inner.redirect_cache),
            ),
        ];

        // Collect circuit breaker states from services that have them.
        let mut circuit_breakers = vec![(
            "ai_provider".to_string(),
            CircuitBreakerHealth {
                state: self
                    .inner
                    .ai_providers
                    .circuit_breaker()
                    .state_name()
                    .to_string(),
            },
        )];

        if let Some(ref email) = self.inner.email {
            circuit_breakers.push((
                "email_smtp".to_string(),
                CircuitBreakerHealth {
                    state: email.circuit_breaker().state_name().to_string(),
                },
            ));
        }

        HealthReport {
            db,
            redis,
            plugins,
            optional,
            circuit_breakers,
        }
    }
}

/// Map an optional service to health status.
fn opt_health<T>(svc: &Option<Arc<T>>) -> ServiceHealth {
    if svc.is_some() {
        ServiceHealth::Healthy
    } else {
        ServiceHealth::Unavailable("not configured".to_string())
    }
}

/// Structured health report for monitoring.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HealthReport {
    /// Database connectivity and pool health.
    pub db: ServiceHealth,
    /// Redis connectivity.
    pub redis: ServiceHealth,
    /// Plugin system health.
    pub plugins: ServiceHealth,
    /// Optional service statuses.
    pub optional: Vec<(String, ServiceHealth)>,
    /// Circuit breaker states for external services.
    pub circuit_breakers: Vec<(String, CircuitBreakerHealth)>,
}

/// Circuit breaker health status.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CircuitBreakerHealth {
    /// Current state: "closed", "open", or "half_open".
    pub state: String,
}

/// Health status for a single service.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "status", content = "detail")]
pub enum ServiceHealth {
    /// Service is fully operational.
    Healthy,
    /// Service is working but with warnings.
    Degraded(String),
    /// Service is not available.
    Unavailable(String),
}
