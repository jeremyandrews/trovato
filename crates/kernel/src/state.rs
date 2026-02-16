//! Application state shared across all handlers.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use redis::Client as RedisClient;
use sqlx::PgPool;

use tracing::info;

use crate::batch::BatchService;
use crate::cache::CacheLayer;
use crate::config::Config;
use crate::config_storage::{ConfigStorage, DirectConfigStorage, StageAwareConfigStorage};
use crate::content::{ContentTypeRegistry, ItemService};
use crate::cron::CronService;
use crate::db;
use crate::file::{FileService, LocalFileStorage};
use crate::form::FormService;
use crate::gather::{CategoryService, GatherService};
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

        // Create gather service and load queries
        let gather = GatherService::new(db.clone(), categories.clone());
        gather
            .load_queries()
            .await
            .context("failed to load gather queries")?;

        // Register blog_listing query if it doesn't exist
        if gather.get_query("blog_listing").is_none() {
            use crate::gather::{
                DisplayFormat, FilterOperator, FilterValue, GatherQuery, PagerConfig,
                QueryDefinition, QueryDisplay, QueryFilter, QuerySort, SortDirection,
            };

            let blog_query = GatherQuery {
                query_id: "blog_listing".to_string(),
                label: "Blog".to_string(),
                description: Some("Recent blog posts".to_string()),
                definition: QueryDefinition {
                    base_table: "item".to_string(),
                    item_type: Some("blog".to_string()),
                    fields: Vec::new(),
                    filters: vec![QueryFilter {
                        field: "status".to_string(),
                        operator: FilterOperator::Equals,
                        value: FilterValue::Integer(1),
                        exposed: false,
                        exposed_label: None,
                    }],
                    sorts: vec![QuerySort {
                        field: "created".to_string(),
                        direction: SortDirection::Desc,
                        nulls: None,
                    }],
                    relationships: Vec::new(),
                    includes: std::collections::HashMap::new(),
                },
                display: QueryDisplay {
                    format: DisplayFormat::List,
                    items_per_page: 10,
                    pager: PagerConfig {
                        enabled: true,
                        ..PagerConfig::default()
                    },
                    empty_text: Some("No blog posts yet.".to_string()),
                    header: None,
                    footer: None,
                },
                plugin: "core".to_string(),
                created: chrono::Utc::now().timestamp(),
                changed: chrono::Utc::now().timestamp(),
            };

            if let Err(e) = gather.register_query(blog_query).await {
                tracing::warn!(error = %e, "failed to register blog_listing query");
            }
        }

        // Register /blog URL alias for the blog listing query
        {
            use crate::models::UrlAlias;
            if let Err(e) =
                UrlAlias::upsert_for_source(&db, "/gather/blog_listing", "/blog", "live", "en")
                    .await
            {
                tracing::warn!(error = %e, "failed to register /blog alias");
            }
        }

        // Register Netgrasp validation content types, queries, roles, and aliases
        register_netgrasp_validation(&db, &content_types, &gather).await;

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

/// Register Netgrasp-style content types, Gather queries, auth roles, and URL aliases.
///
/// This is an Epic 20 validation function: it proves Trovato's existing content model,
/// query engine, auth system, and template rendering can handle a network monitoring
/// application without custom endpoints or schema changes.
async fn register_netgrasp_validation(
    db: &PgPool,
    content_types: &ContentTypeRegistry,
    gather: &GatherService,
) {
    use crate::gather::{
        DisplayFormat, FilterOperator, FilterValue, GatherQuery, PagerConfig, QueryDefinition,
        QueryDisplay, QueryFilter, QuerySort, SortDirection,
    };
    use crate::models::{Role, UrlAlias};

    // ── Content Types ───────────────────────────────────────────────────

    let ng_types: &[(&str, &str, &str, serde_json::Value)] = &[
        (
            "ng_device",
            "Device",
            "Network device tracked by Netgrasp",
            serde_json::json!({
                "fields": [
                    {"name": "mac", "type": "string", "label": "MAC Address", "required": true},
                    {"name": "display_name", "type": "string", "label": "Display Name"},
                    {"name": "hostname", "type": "string", "label": "Hostname"},
                    {"name": "vendor", "type": "string", "label": "Vendor"},
                    {"name": "device_type", "type": "string", "label": "Device Type"},
                    {"name": "os_family", "type": "string", "label": "OS Family"},
                    {"name": "state", "type": "string", "label": "State"},
                    {"name": "last_ip", "type": "string", "label": "Last IP"},
                    {"name": "current_ap", "type": "string", "label": "Current AP"},
                    {"name": "owner_id", "type": "string", "label": "Owner"},
                    {"name": "hidden", "type": "boolean", "label": "Hidden"},
                    {"name": "notify", "type": "boolean", "label": "Notify"},
                    {"name": "baseline", "type": "boolean", "label": "Baseline"}
                ]
            }),
        ),
        (
            "ng_person",
            "Person",
            "Person associated with network devices",
            serde_json::json!({
                "fields": [
                    {"name": "name", "type": "string", "label": "Name", "required": true},
                    {"name": "notes", "type": "text", "label": "Notes"},
                    {"name": "notification_prefs", "type": "string", "label": "Notification Preferences"}
                ]
            }),
        ),
        (
            "ng_event",
            "Event",
            "Network event (device seen, new device, etc.)",
            serde_json::json!({
                "fields": [
                    {"name": "device_id", "type": "string", "label": "Device ID", "required": true},
                    {"name": "event_type", "type": "string", "label": "Event Type", "required": true},
                    {"name": "timestamp", "type": "integer", "label": "Timestamp", "required": true},
                    {"name": "details", "type": "text", "label": "Details"}
                ]
            }),
        ),
        (
            "ng_presence",
            "Presence Session",
            "Device presence session (online period)",
            serde_json::json!({
                "fields": [
                    {"name": "device_id", "type": "string", "label": "Device ID", "required": true},
                    {"name": "start_time", "type": "integer", "label": "Start Time", "required": true},
                    {"name": "end_time", "type": "integer", "label": "End Time"}
                ]
            }),
        ),
        (
            "ng_ip_history",
            "IP History",
            "Historical IP address assignments for devices",
            serde_json::json!({
                "fields": [
                    {"name": "device_id", "type": "string", "label": "Device ID", "required": true},
                    {"name": "ip_address", "type": "string", "label": "IP Address", "required": true},
                    {"name": "first_seen", "type": "integer", "label": "First Seen", "required": true},
                    {"name": "last_seen", "type": "integer", "label": "Last Seen"}
                ]
            }),
        ),
        (
            "ng_location",
            "Location History",
            "Device location history",
            serde_json::json!({
                "fields": [
                    {"name": "device_id", "type": "string", "label": "Device ID", "required": true},
                    {"name": "location", "type": "string", "label": "Location", "required": true},
                    {"name": "start_time", "type": "integer", "label": "Start Time", "required": true},
                    {"name": "end_time", "type": "integer", "label": "End Time"}
                ]
            }),
        ),
    ];

    for (machine_name, label, description, settings) in ng_types {
        if !content_types.exists(machine_name) {
            if let Err(e) = content_types
                .create(machine_name, label, Some(description), settings.clone())
                .await
            {
                tracing::warn!(error = %e, content_type = machine_name, "failed to register ng content type");
            }
        }
    }

    // ── Gather Queries ──────────────────────────────────────────────────

    let now = chrono::Utc::now().timestamp();

    // Device listing with 3 exposed filters
    if gather.get_query("ng_device_list").is_none() {
        let query = GatherQuery {
            query_id: "ng_device_list".to_string(),
            label: "Devices".to_string(),
            description: Some("Network devices tracked by Netgrasp".to_string()),
            definition: QueryDefinition {
                base_table: "item".to_string(),
                item_type: Some("ng_device".to_string()),
                fields: Vec::new(),
                filters: vec![
                    QueryFilter {
                        field: "status".to_string(),
                        operator: FilterOperator::Equals,
                        value: FilterValue::Integer(1),
                        exposed: false,
                        exposed_label: None,
                    },
                    QueryFilter {
                        field: "fields.state".to_string(),
                        operator: FilterOperator::Contains,
                        value: FilterValue::String(String::new()),
                        exposed: true,
                        exposed_label: Some("State".to_string()),
                    },
                    QueryFilter {
                        field: "fields.device_type".to_string(),
                        operator: FilterOperator::Contains,
                        value: FilterValue::String(String::new()),
                        exposed: true,
                        exposed_label: Some("Device Type".to_string()),
                    },
                    QueryFilter {
                        field: "fields.owner_id".to_string(),
                        operator: FilterOperator::Contains,
                        value: FilterValue::String(String::new()),
                        exposed: true,
                        exposed_label: Some("Owner".to_string()),
                    },
                ],
                sorts: vec![QuerySort {
                    field: "fields.display_name".to_string(),
                    direction: SortDirection::Asc,
                    nulls: None,
                }],
                relationships: Vec::new(),
                includes: std::collections::HashMap::new(),
            },
            display: QueryDisplay {
                format: DisplayFormat::Table,
                items_per_page: 50,
                pager: PagerConfig {
                    enabled: true,
                    ..PagerConfig::default()
                },
                empty_text: Some("No devices found.".to_string()),
                header: None,
                footer: None,
            },
            plugin: "core".to_string(),
            created: now,
            changed: now,
        };

        if let Err(e) = gather.register_query(query).await {
            tracing::warn!(error = %e, "failed to register ng_device_list query");
        }
    }

    // Event log with time-range exposed filters
    if gather.get_query("ng_event_log").is_none() {
        let query = GatherQuery {
            query_id: "ng_event_log".to_string(),
            label: "Event Log".to_string(),
            description: Some("Network events and activity log".to_string()),
            definition: QueryDefinition {
                base_table: "item".to_string(),
                item_type: Some("ng_event".to_string()),
                fields: Vec::new(),
                filters: vec![
                    QueryFilter {
                        field: "status".to_string(),
                        operator: FilterOperator::Equals,
                        value: FilterValue::Integer(1),
                        exposed: false,
                        exposed_label: None,
                    },
                    QueryFilter {
                        field: "fields.timestamp".to_string(),
                        operator: FilterOperator::GreaterOrEqual,
                        value: FilterValue::Integer(0),
                        exposed: true,
                        exposed_label: Some("After".to_string()),
                    },
                    QueryFilter {
                        field: "fields.timestamp".to_string(),
                        operator: FilterOperator::LessOrEqual,
                        value: FilterValue::Integer(i64::MAX),
                        exposed: true,
                        exposed_label: Some("Before".to_string()),
                    },
                ],
                sorts: vec![QuerySort {
                    field: "fields.timestamp".to_string(),
                    direction: SortDirection::Desc,
                    nulls: None,
                }],
                relationships: Vec::new(),
                includes: std::collections::HashMap::new(),
            },
            display: QueryDisplay {
                format: DisplayFormat::Table,
                items_per_page: 100,
                pager: PagerConfig {
                    enabled: true,
                    ..PagerConfig::default()
                },
                empty_text: Some("No events recorded.".to_string()),
                header: None,
                footer: None,
            },
            plugin: "core".to_string(),
            created: now,
            changed: now,
        };

        if let Err(e) = gather.register_query(query).await {
            tracing::warn!(error = %e, "failed to register ng_event_log query");
        }
    }

    // ── URL Aliases ─────────────────────────────────────────────────────

    for (source, alias) in [
        ("/gather/ng_device_list", "/devices"),
        ("/gather/ng_event_log", "/events"),
    ] {
        if let Err(e) = UrlAlias::upsert_for_source(db, source, alias, "live", "en").await {
            tracing::warn!(error = %e, alias, "failed to register ng alias");
        }
    }

    // ── Auth Roles ──────────────────────────────────────────────────────

    let ng_type_names = [
        "ng_device",
        "ng_person",
        "ng_event",
        "ng_presence",
        "ng_ip_history",
        "ng_location",
    ];

    // network_admin: full CRUD on all ng_* types
    if Role::find_by_name(db, "network_admin")
        .await
        .ok()
        .flatten()
        .is_none()
    {
        match Role::create(db, "network_admin").await {
            Ok(role) => {
                let mut perms = vec!["access content".to_string()];
                for t in &ng_type_names {
                    perms.push(format!("create {} content", t));
                    perms.push(format!("edit any {} content", t));
                    perms.push(format!("delete any {} content", t));
                }
                for perm in &perms {
                    if let Err(e) = Role::add_permission(db, role.id, perm).await {
                        tracing::warn!(error = %e, perm, "failed to add permission to network_admin");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to create network_admin role"),
        }
    }

    // ng_viewer: read-only access
    if Role::find_by_name(db, "ng_viewer")
        .await
        .ok()
        .flatten()
        .is_none()
    {
        match Role::create(db, "ng_viewer").await {
            Ok(role) => {
                if let Err(e) = Role::add_permission(db, role.id, "access content").await {
                    tracing::warn!(error = %e, "failed to add permission to ng_viewer");
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to create ng_viewer role"),
        }
    }
}
