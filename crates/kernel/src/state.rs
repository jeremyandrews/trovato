//! Application state shared across all handlers.

use crate::redis_conn::BoundedRedis;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use redis::Client as RedisClient;
use sqlx::PgPool;

use tracing::{error, info, warn};

use crate::middleware::language::LanguageSet;

use crate::assistant::AssistantRegistry;
use crate::batch::BatchService;
use crate::cache::CacheLayer;
use crate::config::{CacheConfig, Config};
use crate::config_storage::{ConfigStorage, DirectConfigStorage};
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
use crate::plugin::{PluginConfig, PluginRuntime, status as plugin_status};
use crate::search::SearchService;
use crate::search::sentiment::SentimentQueue;
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
    /// Readiness and draining state for the health probes.
    probes: Arc<crate::routes::health::Probes>,

    /// Plugin search path on disk.
    plugins_dirs: Vec<PathBuf>,

    /// The page builder components pages may use: the standard set and the
    /// components of the plugins enabled at startup.
    page_builder_components: Arc<crate::content::page_builder_components::ComponentRegistry>,

    /// Each plugin enabled at startup that has a `static/` directory, by name:
    /// served at `/static/plugins/<name>/`.
    plugin_static_dirs: std::collections::HashMap<String, PathBuf>,

    /// Set of enabled plugin names (mutable via admin UI).
    ///
    /// Uses `parking_lot::RwLock` rather than `std::sync::RwLock` because:
    /// - No poisoning: a panic in a writer won't permanently wedge every reader.
    /// - Shorter critical sections avoid blocking Tokio worker threads.
    ///
    /// Behind an `Arc` so the audit trail sees the same set.
    enabled_plugins: Arc<parking_lot::RwLock<std::collections::HashSet<String>>>,

    /// The one audit helper the write services record through.
    audit_trail: services::audit::AuditTrail,

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

    /// `/api/openapi.json`, serialized once beside the menu registry it reads.
    /// Both are fixed for the life of the process, so the document changes
    /// exactly when the plugin set does: at the next start.
    openapi: Arc<str>,

    /// Assistant scope registry, built once at boot from `tap_assistant_scopes`.
    assistant_scopes: Arc<AssistantRegistry>,
    /// Permissions plugins declared through `tap_perm`, collected once at boot.
    plugin_permissions: Arc<crate::plugin::permission_registry::PluginPermissionRegistry>,

    /// Admin sidebar links and dashboard cards plugins declared through
    /// `tap_admin_nav`, collected once at boot.
    plugin_admin_nav: Arc<crate::plugin::admin_nav_registry::AdminNavRegistry>,

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

    /// The follow-up sentiment classifier's queue and worker.
    sentiment: Arc<SentimentQueue>,

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

    /// The site's languages and the negotiator chain built from them
    /// ([`LanguageSet`]). Replaced whole by [`AppState::reload_languages`]
    /// when the languages page changes anything, so a language added there
    /// is served at once (B161); before, the set was frozen at startup.
    languages: parking_lot::RwLock<Arc<LanguageSet>>,

    /// User service for user CRUD with tap integration and caching.
    users: Arc<services::user::UserService>,

    /// Role service for role/permission management with cache invalidation.
    roles: Arc<services::role::RoleService>,

    /// Tile rendering service.
    tiles: Arc<services::tile::TileService>,

    /// URL alias writes.
    aliases: Arc<services::url_alias::UrlAliasService>,

    /// Redirect writes: loop check, cache bump and audit entry, in one place.
    redirects: Arc<services::redirect::RedirectService>,

    /// Menu link writes.
    menu_links: Arc<services::menu_link::MenuLinkService>,

    /// The kernel-internal append-only security audit stream (Epic 4).
    ///
    /// Non-optional kernel infrastructure, distinct from the plugin-gated
    /// `audit` field below: authentication, credential, session, and recovery
    /// events all land here so incident response has one place to look.
    security_audit: Arc<crate::audit::SecurityAudit>,

    /// The WebAuthn relying party (FR-7a / D-34).
    ///
    /// Kernel infrastructure, not a plugin concern: a WASM plugin can neither
    /// hold ceremony state nor speak the browser WebAuthn API. `None` when
    /// `SITE_URL` cannot be a WebAuthn relying party (a bare IP address, most
    /// commonly — browsers refuse WebAuthn on an IP origin anyway) — passkeys
    /// are disabled rather than refusing to start. See
    /// [`services::webauthn::build_webauthn`].
    webauthn: Option<Arc<webauthn_rs::Webauthn>>,

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

    /// What the email service above was built from, without the secrets, for
    /// the site settings page to show.
    smtp_summary: crate::config::SmtpSummary,

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

    /// The configured front page path, cached, and the application `/` is
    /// dispatched through (`routes::front`).
    front_page: crate::routes::front::FrontPageConfig,

    /// Comment service (available when comments plugin is enabled).
    ///
    /// Uses `OnceLock` rather than `Option` so the service can be
    /// late-initialized when the plugin is enabled after `AppState`
    /// construction (e.g. in test helpers via `init_comments_service()`).
    comments: OnceLock<Arc<services::comment::CommentService>>,
}

/// Read every stored `rate_limit.*` override (the bucket defaults,
/// `rate_limit.<bucket>`, and the per-role counts, `rate_limit.<bucket>.<role>`)
/// for the default tenant, as `(key, value)` pairs.
///
/// A read failure is not fatal: the caller keeps the compiled defaults, because
/// refusing to boot over an unreadable *optional* setting would turn a tuning
/// knob into a liveness dependency.
async fn load_rate_limit_overrides(db: &PgPool) -> Vec<(String, String)> {
    // One query for every key under the prefix: the bucket defaults and the
    // per-role counts alike. The role half of a key is open-ended, so there is
    // no list of keys to ask for one by one.
    let rows = sqlx::query_as::<_, (String, serde_json::Value)>(
        "SELECT key, value FROM site_config WHERE key LIKE 'rate\\_limit.%' AND tenant_id = $1",
    )
    .bind(crate::models::tenant::DEFAULT_TENANT_ID)
    .fetch_all(db)
    .await;
    match rows {
        Ok(rows) => rows
            .into_iter()
            .map(|(key, value)| match value {
                serde_json::Value::String(s) => (key, s),
                other => (key, other.to_string()),
            })
            .collect(),
        Err(e) => {
            warn!(error = %e, "failed to read rate limit overrides, using the defaults");
            Vec::new()
        }
    }
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

        // Run migrations, unless they run as their own step (`trovato migrate`);
        // then `serve` has already waited until the schema is ready.
        if config.migrate_on_start {
            db::run_migrations(&db)
                .await
                .context("failed to run migrations")?;
        }

        // Create Redis client
        let redis = RedisClient::open(config.redis_url.as_str())
            .context("failed to create Redis client")?;

        // Create the rate limiter here, before anything that dispatches to a
        // plugin: the per-plugin `mail` bucket is checked inside the host
        // function, so every dispatch path has to be able to carry the
        // limiter, the boot-time `tap_install` spawn below included.
        //
        // Trusted proxies (whose X-Forwarded-For is believed) are parsed once by
        // `Config::from_env` from TRUSTED_PROXIES; an empty list ⇒ trust none.
        // The limiter carries them because `resolve_client_ip` reads them there.
        //
        // Each bucket starts at its compiled default and is then overridden by
        // `TROVATO_RATE_LIMIT_<BUCKET>` or the `rate_limit.<bucket>` site config
        // key, environment first. Resolved once here: a site config edit takes
        // effect on the next restart, which is the same as every other setting
        // `AppState::new` reads.
        //
        // Per-role counts (`TROVATO_RATE_LIMIT_<BUCKET>__<ROLE>`,
        // `rate_limit.<bucket>.<role>`) are read in the same pass, with the same
        // precedence.
        let mut rate_limit_config = RateLimitConfig::default();
        let stored_limits = load_rate_limit_overrides(&db).await;
        rate_limit_config.apply_overrides(&|key| std::env::var(key).ok(), &|key| {
            stored_limits
                .iter()
                .find(|(stored, _)| stored == key)
                .map(|(_, value)| value.clone())
        });
        let env_limits: Vec<(String, String)> = std::env::vars()
            .filter(|(key, _)| key.starts_with("TROVATO_RATE_LIMIT_"))
            .collect();
        rate_limit_config.apply_role_overrides(&env_limits, &stored_limits);
        // Uploaded files are static assets too, and `FILES_URL` decides where
        // they are served from, so the bucket has to follow it rather than assume
        // the default.
        let rate_limiter = Arc::new(
            RateLimiter::new(
                redis.clone(),
                rate_limit_config,
                config.trusted_proxies.clone(),
            )
            .with_static_prefixes(&["/static", config.files_url.as_str()]),
        );

        // Test Redis connection
        let mut conn = redis
            .bounded_connection()
            .await
            .context("failed to connect to Redis")?;

        redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
            .context("Redis PING failed")?;

        // Create config storage
        // File service with local storage. Created here because config storage
        // stores the bytes of `file` config entities through it.
        let file_storage = Arc::new(LocalFileStorage::new(
            &config.uploads_dir,
            &config.files_url,
        ));
        let files = Arc::new(FileService::new(db.clone(), file_storage));

        let config_storage: Arc<dyn ConfigStorage> =
            Arc::new(DirectConfigStorage::new(db.clone()).with_files(files.clone()));

        // Create metrics first: every service that owns a cache counts its
        // hits and misses into this one registry, the one `/metrics` encodes.
        let metrics = Arc::new(Metrics::new());

        // Create permission service
        let permissions =
            PermissionService::new(db.clone(), cache_config.ttl_permissions, metrics.clone());

        // Create lockout service
        let lockout =
            LockoutService::new(redis.clone()).with_ip_storage(config.runtime.client_ip_storage);

        // The operator allowlist (`PLUGIN_HTTP_PRIVATE_ALLOW`): one client per
        // entry, built once here and shared (behind an `Arc`) by every
        // dispatch path that issues plugin-influenced HTTP — the request
        // path's `tap_services` below, `tap_install`, and `CronService`'s
        // `tap_cron`/`tap_queue_worker` dispatch.
        let plugin_http_allow = Arc::new(crate::host::http::build_plugin_http_allow_runtime(
            &config.plugin_http_private_allow,
        ));

        // Discover plugins on disk (parse info.toml without compiling WASM)
        let discovered = PluginRuntime::discover_plugins(&config.plugins_dirs).await;

        // An allowlist entry naming a plugin that was never discovered on
        // disk is inert rather than an error: the plugin may simply not be
        // installed on this deployment yet. One warning per start, naming
        // every such entry, so an operator sees a typo rather than silently
        // wondering why the plugin never reaches its destination.
        let allow_unknown_plugins: Vec<&str> = config
            .plugin_http_private_allow
            .entries()
            .iter()
            .map(|entry| entry.plugin.as_str())
            .filter(|name| !discovered.contains_key(*name))
            .collect();
        if !allow_unknown_plugins.is_empty() {
            warn!(
                plugins = ?allow_unknown_plugins,
                "PLUGIN_HTTP_PRIVATE_ALLOW names a plugin that is not installed; the entry is inert"
            );
        }

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
        let enabled_plugins = Arc::new(parking_lot::RwLock::new(enabled_set.clone()));
        // What every write service audits through: a content row when
        // `trovato_audit_log` is enabled (read from the shared set at each
        // write), a security event always.
        let audit_trail = services::audit::AuditTrail::new(
            db.clone(),
            config.runtime.client_ip_storage,
            enabled_plugins.clone(),
        );

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

        // One line naming every plugin that runs SQL the kernel does not
        // compile, so an operator reading the boot log sees each grant (D-60).
        let raw_sql = plugin_runtime.raw_sql_holders();
        if !raw_sql.is_empty() {
            tracing::warn!(
                plugins = %raw_sql.join(", "),
                "plugins holding raw_sql: SQL the kernel does not compile, with no viewer, stage or language applied"
            );
        }

        // The enabled plugins' asset directories, by name: page builder
        // components under `components/`, templates under `templates/`,
        // stylesheets and other files under `static/`. Read at startup, like
        // the plugins themselves, so a plugin enabled or disabled later
        // changes them on the next restart.
        let mut enabled_plugin_dirs: Vec<(String, PathBuf)> = discovered
            .iter()
            .filter(|(name, _)| enabled_set.contains(*name))
            .map(|(name, (_, dir))| (name.clone(), dir.clone()))
            .collect();
        enabled_plugin_dirs.sort();
        let page_builder_components = Arc::new(
            crate::content::page_builder_components::ComponentRegistry::load(
                &enabled_plugin_dirs
                    .iter()
                    .map(|(name, dir)| {
                        crate::content::page_builder_components::PluginComponentSource {
                            name: name.clone(),
                            dir: dir.clone(),
                        }
                    })
                    .collect::<Vec<_>>(),
            )
            .context("failed to load page builder components")?,
        );
        let plugin_static_dirs: std::collections::HashMap<String, PathBuf> = enabled_plugin_dirs
            .iter()
            .filter(|(_, dir)| dir.join("static").is_dir())
            .map(|(name, dir)| (name.clone(), dir.join("static")))
            .collect();
        let plugin_template_dirs: Vec<PathBuf> = enabled_plugin_dirs
            .iter()
            .map(|(_, dir)| dir.join("templates"))
            .filter(|dir| dir.is_dir())
            .collect();

        // Run pending plugin migrations for enabled plugins
        let enabled_discovered: std::collections::HashMap<
            String,
            (crate::plugin::PluginInfo, std::path::PathBuf),
        > = discovered
            .into_iter()
            .filter(|(name, _)| enabled_set.contains(name))
            .collect();
        if config.migrate_on_start {
            let mut lock = crate::migrate::MigrationLock::acquire(&db).await?;
            crate::migrate::run_plugins(&mut lock, &enabled_discovered).await?;
            lock.release().await?;
        }

        let plugin_runtime = Arc::new(plugin_runtime);

        // Create tap registry
        let tap_registry = Arc::new(TapRegistry::from_plugins(&plugin_runtime));

        // Create tap dispatcher
        let tap_dispatcher = Arc::new(TapDispatcher::new(
            plugin_runtime.clone(),
            tap_registry.clone(),
        ));

        // Initialize email service (conditionally, when SMTP_HOST is set).
        //
        // Built here, before the first tap dispatch (`tap_install` below) and the
        // tap services template, because the `mail` host interface is served
        // from both: a plugin sending mail uses this same service, and therefore
        // the same SMTP transport, `from` address and circuit breaker the
        // kernel's own mail uses, on every dispatch path.
        let email = config.smtp_host.as_ref().and_then(|host| {
            match services::mail_transport::SmtpTransport::new(
                host,
                config.smtp_port,
                config.smtp_username.as_deref(),
                config.smtp_password.as_deref(),
                &config.smtp_encryption,
            ) {
                Ok(transport) => {
                    info!(host = %host, port = config.smtp_port, "SMTP email service configured");
                    Some(Arc::new(services::email::EmailService::new(
                        db.clone(),
                        Arc::new(transport),
                        config.smtp_from_email.clone(),
                        config.site_url.clone(),
                        config.mail.clone(),
                    )))
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to initialize email service");
                    None
                }
            }
        });

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
        // The item service is built further down. The install task waits for
        // it, so `item-api` calls from `tap_install` are decided rather than
        // refused for want of one. The gather service travels with it, so
        // `query` from `tap_install` runs rather than answering
        // `ERR_NO_SERVICES`.
        let mut tap_install_items: Option<
            tokio::sync::oneshot::Sender<(Arc<ItemService>, Arc<GatherService>)>,
        > = None;
        if !pending.is_empty() {
            let (items_tx, items_rx) = tokio::sync::oneshot::channel();
            tap_install_items = Some(items_tx);
            let tap_install_db = db.clone();
            let tap_install_dispatcher = tap_dispatcher.clone();
            let tap_install_limiter = rate_limiter.clone();
            let tap_install_email = email.clone();
            let tap_install_http_allow = plugin_http_allow.clone();
            tokio::spawn(async move {
                let http = crate::host::http::build_outbound_client();
                // The sender drops only when `AppState::new` failed before
                // building the item service. Claim nothing then: a claim
                // stands even when its dispatch fails, so dispatching from a
                // server that is not starting would spend each hook the next
                // healthy start should run.
                let Ok((items, gather)) = items_rx.await else {
                    warn!(
                        pending = pending.len(),
                        "startup failed; tap_install left pending for the next start"
                    );
                    return;
                };
                for plugin_name in &pending {
                    // Claim before dispatching: with several servers starting
                    // together, only the one whose claim lands runs it.
                    match plugin_status::claim_tap_install(&tap_install_db, plugin_name).await {
                        Ok(true) => {}
                        Ok(false) => {
                            info!(plugin = %plugin_name, "tap_install already claimed by another server");
                            continue;
                        }
                        Err(e) => {
                            error!(
                                plugin = %plugin_name,
                                error = %e,
                                "failed to claim tap_install; skipping it"
                            );
                            continue;
                        }
                    }
                    let mut install_services = RequestServices::for_background(
                        tap_install_db.clone(),
                        None,
                        None,
                        http.clone(),
                    )
                    .with_plugin_runtime(tap_install_dispatcher.runtime().clone())
                    .with_rate_limiter(tap_install_limiter.clone())
                    .with_http_allow(tap_install_http_allow.clone());
                    if let Some(ref email) = tap_install_email {
                        install_services = install_services.with_email(email.clone());
                    }
                    install_services.set_item_service(&items);
                    install_services.set_gather_service(&gather);
                    let install_state =
                        RequestState::new(UserContext::anonymous(), install_services);
                    // dispatch_to_plugin returns None when the plugin does not
                    // export tap_install (harmless) OR when the WASM call fails
                    // (the dispatcher already logs an error in that case). The
                    // claim stands either way — to retry, set
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
        let openapi: Arc<str> = crate::routes::route_metadata::RouteRegistry::openapi_document(
            &menu_registry.all().cloned().collect::<Vec<_>>(),
        )
        .to_string()
        .into();

        // Plugin permissions, collected the same way and for the same reason: a
        // permission declaration is a constant, so it is gathered once here
        // rather than asked for during a request. Dispatched without services,
        // like `tap_menu`.
        //
        // Unlike the other two registries this one is also written to the
        // database, because it has a consumer that is not this process:
        // `config import` validates a role file's permission list from a CLI
        // that has a pool and no `AppState`. Until this dispatch existed, a
        // plugin's permissions were in no list the kernel could consult, so they
        // could not be granted from the grid or named in a config file, and the
        // grid's save revoked them.
        let plugin_permissions = {
            let perm_state = RequestState::without_services(UserContext::anonymous());
            let results = tap_dispatcher.dispatch("tap_perm", "{}", perm_state).await;
            let registry =
                crate::plugin::permission_registry::PluginPermissionRegistry::from_tap_results(
                    results
                        .into_iter()
                        .map(|r| (r.plugin_name, r.output))
                        .collect(),
                );
            if !registry.is_empty() {
                info!(count = registry.len(), "registered plugin permissions");
            }
            // A failure to store is logged and not fatal: the site boots with
            // the grid showing the kernel's permissions only, which is what it
            // did before this existed.
            if let Err(e) = crate::plugin::permission_registry::persist(&db, &registry).await {
                error!(error = %e, "failed to store declared plugin permissions");
            }
            Arc::new(registry)
        };

        // Admin sidebar links and dashboard cards, collected the same way and
        // for the same reason: a declaration is a constant, so it is gathered
        // once here rather than asked for during a request. Dispatched
        // without services, like `tap_menu` and `tap_perm` — a plugin names a
        // path and a permission, it does not need a DB handle to do that.
        let plugin_admin_nav = {
            let admin_nav_state = RequestState::without_services(UserContext::anonymous());
            let results = tap_dispatcher
                .dispatch("tap_admin_nav", "{}", admin_nav_state)
                .await;
            let registry = crate::plugin::admin_nav_registry::AdminNavRegistry::from_tap_results(
                results
                    .into_iter()
                    .map(|r| (r.plugin_name, r.output))
                    .collect(),
            );
            if !registry.is_empty() {
                info!(count = registry.len(), "registered plugin admin nav links");
            }
            Arc::new(registry)
        };

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
            metrics.clone(),
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
        let categories =
            CategoryService::new(db.clone(), cache_config.ttl_categories, metrics.clone());
        categories.set_audit_trail(audit_trail.clone());

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
            metrics.clone(),
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
            let locale_service = services::locale::LocaleService::new(db.clone(), metrics.clone());
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
        //
        // The enabled plugins' `templates/` directories come first, so they
        // are the lowest precedence: a plugin adds templates (a page builder
        // component's, for one) and cannot replace the kernel's, and a theme
        // on TEMPLATES_DIR overrides a plugin's as it overrides the kernel's.
        let template_dirs: Vec<PathBuf> = plugin_template_dirs
            .iter()
            .chain(config.templates_dirs.iter())
            .cloned()
            .collect();
        info!(template_dirs = ?template_dirs, "loading templates from search path");
        let themes = crate::theme::registry::discover(&config.themes_dirs);
        info!(themes = themes.len(), themes_dirs = ?config.themes_dirs, "discovered themes");
        // A kernel, plugin or app template that does not parse stops startup
        // with an error naming each broken file and Tera's message (B138). It
        // used to log a warning and carry on with an empty engine, so every
        // page rendered untemplated and the cause was one warning line. A
        // deployment theme that does not parse is refused instead, in
        // `with_themes`, and the site keeps the stock theme.
        let theme = ThemeEngine::new(&template_dirs, locale.clone(), metrics.clone())
            .inspect_err(|e| {
                tracing::error!(
                    error = %e,
                    "templates on the search path do not load; the server cannot start"
                );
            })
            .context("failed to load templates from the search path")?
            .with_components(page_builder_components.clone())
            .context("failed to load page builder components")?
            .with_themes(themes, &template_dirs, locale.clone());
        let theme = Arc::new(theme);

        // Create form service
        let forms = Arc::new(FormService::new(
            db.clone(),
            tap_dispatcher.clone(),
            theme.clone(),
        ));

        // Create cache layer (Moka L1 + Redis L2)
        let cache = CacheLayer::new(redis.clone(), metrics.clone());

        // Create search service
        let search = Arc::new(SearchService::new(db.clone(), cache.clone()));
        let sentiment = Arc::new(SentimentQueue::start(
            search.clone(),
            ai_providers.clone(),
            db.clone(),
        ));

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
        .with_field_access_cache(field_access_cache)
        // Arms the per-plugin `mail` bucket on every request-path dispatch; the
        // cron and queue-worker paths get it from `CronService` below.
        .with_rate_limiter(rate_limiter.clone())
        .with_http_allow(plugin_http_allow.clone());
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
            metrics.clone(),
        ));
        items.set_page_builder_components(page_builder_components.clone());
        items.set_audit_trail(audit_trail.clone());

        // Late-bind the item-access seam into the tap services template, so
        // the `item-api` host functions can decide a request-scoped call as the
        // user the plugin is acting for.
        //
        // After `ItemService::new`, necessarily: the service is built *from*
        // this template. That is also why the template holds the handle behind
        // a shared cell and holds it weakly — the clone the service took a few
        // lines above sees this binding, and neither side keeps the other
        // alive.
        tap_services.set_item_service(&items);

        // And the gather service, for the `query` host interface, by the same
        // shared, weak cell.
        tap_services.set_gather_service(&gather);

        // Late-bind the item-access seam into the gather service (Story 3.4):
        // gather is constructed before ItemService, so it holds a OnceLock the
        // item service fills here. Every gather read path now runs the shared
        // check_access + field-access seam over its result page.
        gather.set_item_service(items.clone());

        // Late-bind the lightweight-record registry into gather (P11g / D-54), so
        // a gather naming a `record_type` resolves it to its table + field map.
        gather.set_record_types(record_types.clone());

        // Create cron service with file service for proper cleanup
        let mut cron = CronService::with_file_service(redis.clone(), db.clone(), files.clone());

        // The item-access seam, so the Pagefind export is filtered as the
        // anonymous visitor who downloads it. The one `ItemService`, not a
        // second one: a second would carry its own field-access cache.
        cron.set_item_service(items.clone());
        // And the gather service, so `query` from `tap_cron` and
        // `tap_queue_worker` runs on the same one request dispatch binds above.
        cron.set_gather_service(gather.clone());
        // Same allowlist runtime `tap_services` carries, so `tap_cron` and
        // `tap_queue_worker` dispatch honor the same per-plugin grants a
        // request-path call does.
        cron.set_http_allow(plugin_http_allow.clone());

        // Create batch service
        let batch = Arc::new(BatchService::new(redis.clone()));

        // Create stage service
        let stage = Arc::new(
            StageService::new(db.clone(), cache.clone()).with_audit_trail(audit_trail.clone()),
        );

        // Create user service
        let users = Arc::new(services::user::UserService::new(
            db.clone(),
            tap_dispatcher.clone(),
            tap_services.clone(),
            cache_config.ttl_users,
            metrics.clone(),
            audit_trail.clone(),
        ));

        // Late-bind the user service into gather (B166): gather is constructed
        // before UserService, so it holds a OnceLock this fills, same as the
        // item-access seam above.
        gather.set_user_service(users.clone());

        // Only now is the install task handed both. This is the last of
        // gather's late bindings (item access, record types, users), and the
        // task runs on another worker the moment it is woken: handed gather any
        // earlier, a `query` from `tap_install` could run with a seam still
        // unset, its rows missing `author` or worse. The assertion keeps a
        // later binding from landing below this line unnoticed.
        if let Some(tx) = tap_install_items.take() {
            debug_assert!(
                gather.late_bindings_complete(),
                "gather handed to tap_install before every late binding"
            );
            let _ = tx.send((items.clone(), gather.clone()));
        }

        // Create role service (depends on permission service for cache invalidation)
        let roles = Arc::new(services::role::RoleService::new(
            db.clone(),
            permissions.clone(),
            audit_trail.clone(),
        ));

        // Create tile service
        let tiles = Arc::new(services::tile::TileService::new(
            db.clone(),
            audit_trail.clone(),
        ));
        let aliases = Arc::new(services::url_alias::UrlAliasService::new(
            db.clone(),
            audit_trail.clone(),
        ));
        let redirect_cache = if enabled_set.contains("trovato_redirects") {
            Some(Arc::new(services::redirect::RedirectCache::new()))
        } else {
            None
        };
        let redirects = Arc::new(services::redirect::RedirectService::new(
            db.clone(),
            audit_trail.clone(),
            redirect_cache.clone(),
        ));
        let menu_links = Arc::new(services::menu_link::MenuLinkService::new(
            db.clone(),
            audit_trail.clone(),
        ));

        // The language set and its negotiator chain (languages were loaded
        // earlier for locale).
        let prefix_default =
            crate::models::SiteConfig::get(&db, crate::middleware::language::PREFIX_DEFAULT_KEY)
                .await
                .ok()
                .flatten()
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        let language_set = Arc::new(LanguageSet::new(
            known_languages.clone(),
            default_language.clone(),
            prefix_default,
        ));

        // The kernel-internal security audit stream. Always present: an audit
        // trail that is only sometimes written is not an audit trail.
        let security_audit = Arc::new(crate::audit::SecurityAudit::with_ip_storage(
            db.clone(),
            config.runtime.client_ip_storage,
        ));

        // The WebAuthn relying party, derived from SITE_URL so the RP ID and the
        // origin can never drift apart (a mismatch silently invalidates every
        // registered credential). A SITE_URL that cannot be a relying party — a
        // bare IP address is the common case, since webauthn-rs refuses an IP as
        // an effective domain and no browser allows WebAuthn on an IP origin
        // anyway — disables passkeys for this run rather than failing startup.
        let site_name = crate::models::SiteConfig::site_name(&db)
            .await
            .unwrap_or_else(|_| "Trovato".to_string());
        let webauthn = match services::webauthn::build_webauthn(&config.site_url, &site_name) {
            Ok(w) => Some(Arc::new(w)),
            Err(e) => {
                tracing::warn!(
                    site_url = %config.site_url,
                    error = %e,
                    "SITE_URL cannot be a WebAuthn relying party; passkeys are disabled for \
                     this site. Browsers do not support WebAuthn on an IP address — use a \
                     hostname in SITE_URL to enable passkeys."
                );
                None
            }
        };

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
            Some(Arc::new(services::audit::AuditService::with_ip_storage(
                db.clone(),
                config.runtime.client_ip_storage,
            )))
        } else {
            None
        };

        // Always built: it is a handle on the pool, and its callers ask
        // whether `trovato_content_locking` is enabled when they use it, so a
        // plugin enabled after boot locks without a restart. The cron cleanup
        // below runs only for a plugin enabled at boot, whose table exists.
        let content_lock_enabled = enabled_set.contains("trovato_content_locking");
        let content_lock = Some(Arc::new(services::content_lock::ContentLockService::new(
            db.clone(),
        )));

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
                audit_trail.clone(),
            )));
        }

        // Wire plugin services into cron
        cron.set_plugin_services(
            content_lock.clone().filter(|_| content_lock_enabled),
            audit.clone(),
        );
        cron.set_email_service(email.clone());
        // The per-plugin `mail` bucket has to apply from `tap_cron` and
        // `tap_queue_worker`, not only from a request.
        cron.set_rate_limiter(rate_limiter.clone());
        cron.set_tap_dispatcher(tap_dispatcher.clone());
        cron.set_ai_providers(ai_providers.clone());
        cron.set_ai_budgets(ai_budgets.clone());
        // P11f: the native embed drain writes item embeddings into the same
        // pgvector store the gather read path and sync index path share.
        cron.set_vector_store(vector_store.clone());
        cron.set_search_service(search.clone());
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

        let state = Self {
            inner: Arc::new(AppStateInner {
                runtime: config.runtime.clone(),
                db,
                db_pool_max_connections: config.database_max_connections,
                probes: Arc::new(crate::routes::health::Probes::new(
                    config.health_token.clone(),
                )),
                plugins_dirs: config.plugins_dirs.clone(),
                page_builder_components,
                plugin_static_dirs,
                enabled_plugins,
                audit_trail: audit_trail.clone(),
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
                openapi,
                plugin_permissions,
                plugin_admin_nav,
                assistant_scopes,
                content_types,
                record_types,
                items,
                categories,
                gather,
                feed_links,
                search,
                sentiment,
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
                languages: parking_lot::RwLock::new(language_set),
                users,
                roles,
                tiles,
                aliases,
                redirects,
                menu_links,
                security_audit,
                webauthn,
                session_registry,
                recovery_flows,
                recovery_providers,
                email,
                smtp_summary: config.smtp_summary(),
                audit,
                content_lock,
                image_styles,
                oauth,
                locale,
                redirect_cache,
                front_page: crate::routes::front::FrontPageConfig::default(),
                comments,
            }),
        };

        // The themes in use (`theme::registry`), read once here.
        crate::theme::registry::load_active(state.db(), state.theme()).await;

        // The stored text formats, which every render reads synchronously.
        if let Err(e) = crate::content::text_format::reload(state.db()).await {
            warn!(error = %e, "failed to load text formats; using the built-in definitions");
        }

        // Cache generations (`cache::generation`): writes from here bump, and
        // a bump from any server clears that cache here. A single server runs
        // this too; it then only ever sees its own bumps.
        crate::cache::generation::init(state.redis());
        let watched = state.clone();
        crate::cache::generation::spawn_watcher(state.redis().clone(), move |kind| {
            let state = watched.clone();
            async move { state.clear_local_cache(kind).await }
        });

        Ok(state)
    }

    /// Clear every in-process cache of this server, without bumping: what a
    /// bump of every kind does here. The performance tests' "cold" start.
    pub async fn clear_local_caches(&self) {
        for kind in crate::cache::generation::CacheKind::ALL {
            self.clear_local_cache(kind).await;
        }
    }

    /// Clear this server's cache of `kind`, without bumping: what a bump from
    /// any server, this one included, does here.
    async fn clear_local_cache(&self, kind: crate::cache::generation::CacheKind) {
        use crate::cache::generation::CacheKind;
        match kind {
            CacheKind::Permissions => self.permissions().clear_local(),
            CacheKind::Users => self.users().clear_local(),
            CacheKind::Items => self.items().clear_local(),
            CacheKind::FieldAccess => self.items().flush_field_access_cache(),
            CacheKind::ContentTypes => {
                if let Err(e) = self.content_types().reload_from_db().await {
                    warn!(error = %e, "failed to reload content types after a bump");
                }
            }
            CacheKind::Categories => self.categories().clear_local(),
            CacheKind::Gather => {
                if let Err(e) = self.gather().refresh_local().await {
                    warn!(error = %e, "failed to reload gather queries after a bump");
                }
            }
            CacheKind::Redirects => {
                if let Some(redirects) = self.redirect_cache() {
                    redirects.clear();
                }
            }
            CacheKind::Locale => {
                if let Some(locale) = self.locale()
                    && let Err(e) = locale.reload().await
                {
                    warn!(error = %e, "failed to reload locale strings after a bump");
                }
            }
            CacheKind::Menus => {
                crate::menu::clear_local_config_menus();
                self.gather().clear_outbound_alias_cache();
            }
            CacheKind::Languages => {
                if let Err(e) = self.reload_languages().await {
                    warn!(error = %e, "failed to reload languages after a bump");
                }
            }
            CacheKind::SiteConfig => {
                self.front_page().clear();
                self.gather().clear_site_settings();
                self.items().view_counter().clear();
            }
            CacheKind::Themes => {
                crate::theme::registry::load_active(self.db(), self.theme()).await;
            }
            CacheKind::TextFormats => {
                if let Err(e) = crate::content::text_format::reload(self.db()).await {
                    warn!(error = %e, "failed to reload text formats after a bump");
                }
            }
        }
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

    /// The health probes' readiness and draining state.
    pub fn probes(&self) -> &Arc<crate::routes::health::Probes> {
        &self.inner.probes
    }

    /// Get the configured maximum database pool connections.
    pub fn db_pool_max_connections(&self) -> u32 {
        self.inner.db_pool_max_connections
    }

    /// Get the plugin search path.
    pub fn plugins_dirs(&self) -> &[PathBuf] {
        &self.inner.plugins_dirs
    }

    /// The page builder component registry: the standard components and
    /// those of the plugins enabled at startup.
    pub fn page_builder_components(
        &self,
    ) -> &Arc<crate::content::page_builder_components::ComponentRegistry> {
        &self.inner.page_builder_components
    }

    /// The `static/` directory of plugin `name`, if it was enabled at startup
    /// and has one.
    pub fn plugin_static_dir(&self, name: &str) -> Option<&PathBuf> {
        self.inner.plugin_static_dirs.get(name)
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
    /// Nothing in a running server calls this: the admin page and the CLI
    /// change only the stored status, which each server reads when it starts.
    /// Tests use it to flip a plugin without a restart.
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

    /// The serialized `/api/openapi.json` document.
    pub fn openapi(&self) -> &Arc<str> {
        &self.inner.openapi
    }

    /// Get the assistant scope registry.
    pub fn assistant_scopes(&self) -> &Arc<AssistantRegistry> {
        &self.inner.assistant_scopes
    }

    /// The permissions plugins declared through `tap_perm`.
    ///
    /// Read by the permission grid so a plugin's permissions can be granted
    /// there. `config import` reads the same set from the database instead,
    /// because it runs in its own process.
    pub fn plugin_permissions(
        &self,
    ) -> &Arc<crate::plugin::permission_registry::PluginPermissionRegistry> {
        &self.inner.plugin_permissions
    }

    /// The admin pages plugins declared through `tap_admin_nav`.
    ///
    /// Read by [`crate::routes::admin_nav::sidebar`],
    /// [`crate::routes::admin_nav::dashboard`] and the other readers of the
    /// admin registry, merged with the kernel's own
    /// [`crate::routes::admin_nav::LINKS`] under the same permission and
    /// plugin-enablement filter.
    pub fn plugin_admin_nav(&self) -> &Arc<crate::plugin::admin_nav_registry::AdminNavRegistry> {
        &self.inner.plugin_admin_nav
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
    /// The storage config export and import use, for tests that drive them
    /// against the running site's database.
    pub fn config_storage(&self) -> &Arc<dyn ConfigStorage> {
        &self.inner.config_storage
    }

    /// Get the search service.
    pub fn search(&self) -> &Arc<SearchService> {
        &self.inner.search
    }

    /// Get the follow-up sentiment queue.
    pub fn sentiment(&self) -> &Arc<SentimentQueue> {
        &self.inner.sentiment
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

    /// The site's languages as this server last read them.
    pub fn languages(&self) -> Arc<LanguageSet> {
        self.inner.languages.read().clone()
    }

    /// The known language codes, in weight order.
    pub fn known_languages(&self) -> Arc<[String]> {
        self.languages().known()
    }

    /// The default language's code.
    pub fn default_language(&self) -> String {
        self.languages().default_language().to_string()
    }

    /// Read the languages again from the database and serve them from the
    /// next request: what a change on the languages page, here or on another
    /// server, does (`CacheKind::Languages`).
    pub async fn reload_languages(&self) -> Result<()> {
        let set = LanguageSet::load(self.db())
            .await
            .context("failed to reload languages")?;
        *self.inner.languages.write() = Arc::new(set);
        Ok(())
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

    /// Get the URL alias write service.
    pub fn aliases(&self) -> &Arc<services::url_alias::UrlAliasService> {
        &self.inner.aliases
    }

    /// Get the redirect write service.
    pub fn redirects(&self) -> &Arc<services::redirect::RedirectService> {
        &self.inner.redirects
    }

    /// Get the menu link write service.
    pub fn menu_links(&self) -> &Arc<services::menu_link::MenuLinkService> {
        &self.inner.menu_links
    }

    /// Get the email service (if SMTP is configured).
    pub fn email(&self) -> Option<&Arc<services::email::EmailService>> {
        self.inner.email.as_ref()
    }

    /// The SMTP configuration the email service was built from, without the
    /// username or password values.
    pub fn smtp_summary(&self) -> &crate::config::SmtpSummary {
        &self.inner.smtp_summary
    }

    /// The kernel-internal append-only security audit stream (Epic 4).
    ///
    /// Always available. This is where authentication, credential, session, and
    /// recovery events go — not [`AppState::audit`], which is the optional
    /// `trovato_audit_log` plugin's content-CRUD log.
    pub fn security_audit(&self) -> &Arc<crate::audit::SecurityAudit> {
        &self.inner.security_audit
    }

    /// The WebAuthn relying party (FR-7a / D-34), or `None` when `SITE_URL`
    /// cannot be a relying party (a bare IP address) and passkeys are disabled
    /// for this run.
    pub fn webauthn(&self) -> Option<&Arc<webauthn_rs::Webauthn>> {
        self.inner.webauthn.as_ref()
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

    /// The audit helper every write service records through.
    pub fn audit_trail(&self) -> &services::audit::AuditTrail {
        &self.inner.audit_trail
    }

    /// Get the audit service (if audit_log plugin is enabled).
    pub fn audit(&self) -> Option<&Arc<services::audit::AuditService>> {
        self.inner.audit.as_ref()
    }

    /// The content lock service. Always present; a caller asks whether
    /// `trovato_content_locking` is enabled before using it, as the routes
    /// gated on the plugin and the item edit form do.
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

    /// The configured front page path, cached (`routes::front`).
    pub fn front_page(&self) -> &crate::routes::front::FrontPageConfig {
        &self.inner.front_page
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
                self.inner.audit_trail.clone(),
            )));
    }

    /// Check if PostgreSQL is healthy.
    pub async fn postgres_healthy(&self) -> bool {
        db::check_health(&self.inner.db).await
    }

    /// Check if Redis is healthy.
    pub async fn redis_healthy(&self) -> bool {
        let Ok(mut conn) = self.inner.redis.bounded_connection().await else {
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

        if let Some(state) = self
            .inner
            .email
            .as_ref()
            .and_then(|email| email.transport().breaker_state())
        {
            circuit_breakers.push((
                "email_smtp".to_string(),
                CircuitBreakerHealth {
                    state: state.to_string(),
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
