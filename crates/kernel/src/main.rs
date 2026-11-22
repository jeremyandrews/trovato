//! Trovato Kernel
//!
//! HTTP server, plugin runtime, and core services.

// Many models, services, and types expose APIs that are used by plugins, tests,
// or routes not yet wired into the binary. Suppress dead_code and unused re-exports
// for the binary target; the lib target (lib.rs) maintains stricter checking.
#![allow(dead_code, unused_imports)]

mod assistant;
mod audit;
mod batch;
mod cache;
mod circuit_breaker;
mod config;
mod config_storage;
mod content;
mod cron;
mod db;
mod error;
mod file;
mod form;
mod gather;
mod host;
mod lockout;
mod menu;
mod metrics;
mod middleware;
mod models;
mod net_policy;
mod permissions;
mod plugin;
mod recovery;
mod routes;
mod search;
mod services;
mod session;
mod stage;
mod state;
mod system_log;
mod tap;
mod theme;
mod update_status;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use clap::{Parser, Subcommand};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tower_sessions::Session;
use tower_sessions::cookie::SameSite;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, Layer as _, layer::SubscriberExt, util::SubscriberInitExt};

use crate::cache::generation::CacheKind;
use crate::config::Config;
use crate::state::AppState;

#[derive(Parser)]
// `version` reads CARGO_PKG_VERSION, which is the one project version from the
// workspace Cargo.toml, so `trovato --version` cannot drift from the release.
#[command(name = "trovato", about = "Trovato", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the HTTP server (default).
    Serve,
    /// Plugin management commands.
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
    /// Configuration export/import commands.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// User management commands.
    User {
        #[command(subcommand)]
        action: UserAction,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Export all config to YAML files.
    Export {
        /// Output directory.
        #[arg(default_value = "config")]
        dir: String,
        /// Remove stale .yml config files after exporting.
        #[arg(long)]
        clean: bool,
    },
    /// Import config from YAML files.
    Import {
        /// Input directory.
        #[arg(default_value = "config")]
        dir: String,
        /// Validate files without writing to the database.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum UserAction {
    /// Reset a user's password.
    ResetPassword {
        /// Username.
        username: String,
        /// New password (min 12 characters). If omitted, reads from stdin.
        password: Option<String>,
    },
    /// Give a user a role.
    RoleAdd {
        /// Username.
        username: String,
        /// Role name.
        role: String,
    },
    /// Take a role away from a user.
    RoleRemove {
        /// Username.
        username: String,
        /// Role name.
        role: String,
    },
    /// List the roles a user holds.
    Roles {
        /// Username.
        username: String,
    },
}

#[derive(Subcommand)]
enum PluginAction {
    /// Scaffold a new plugin in the plugins/ directory.
    New {
        /// Plugin machine name (snake_case, e.g. my_plugin).
        name: String,
    },
    /// List discovered plugins and their status.
    List,
    /// Install a plugin (run migrations, set enabled).
    Install {
        /// Plugin machine name.
        name: String,
    },
    /// Run pending migrations for a plugin (or all plugins).
    Migrate {
        /// Plugin name. If omitted, runs migrations for all plugins.
        name: Option<String>,
    },
    /// Enable a plugin.
    Enable {
        /// Plugin machine name.
        name: String,
    },
    /// Disable a plugin.
    Disable {
        /// Plugin machine name.
        name: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    // Initialize tracing. The site log's rows wait in a channel until the
    // server has a database pool to write them to.
    let log_receiver = init_tracing();

    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::Serve) => run_server(log_receiver).await,
        Some(Commands::Plugin { action }) => run_plugin_command(action).await,
        Some(Commands::Config { action }) => run_config_command(action).await,
        Some(Commands::User { action }) => run_user_command(action).await,
    }
}

// path_alias_fallback lives in middleware::path_alias and is reused by tests.

/// Run the HTTP server (original startup path).
async fn run_server(log_receiver: system_log::SystemLogReceiver) -> Result<()> {
    info!("Starting Trovato kernel");

    // Load configuration from environment
    let config = Config::from_env().context("failed to load configuration")?;
    info!(port = config.port, "Configuration loaded");

    // Refuse to start on an uploads directory this server cannot use.
    crate::file::check_writable(&config.uploads_dir).await?;

    // Initialize application state (database connections, etc.)
    let state = AppState::new(&config)
        .await
        .context("failed to initialize application state")?;

    info!("Database and Redis connections established");

    // The site log (`/admin/reports/log`): warnings and errors from here on
    // are written to `system_log`.
    system_log::spawn_writer(state.db().clone(), log_receiver);

    // Create a cancellation token for coordinated shutdown of background tasks.
    let shutdown_token = tokio_util::sync::CancellationToken::new();

    // Spawn background task to monitor database pool utilization
    {
        let db = state.db().clone();
        let token = shutdown_token.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let size = db.size();
                        let idle = db.num_idle() as u32;
                        let active = size.saturating_sub(idle);
                        let pct = (active * 100).checked_div(size).unwrap_or(0);
                        if pct >= 80 {
                            warn!(
                                active = active,
                                total = size,
                                pct = pct,
                                "Database pool utilization at {pct}% ({active}/{size} connections active)"
                            );
                        }
                    }
                    _ = token.cancelled() => {
                        info!("DB pool monitor shutting down");
                        break;
                    }
                }
            }
        });
    }

    // Flush search analytics from memory to the database every 30 seconds, and
    // once more on shutdown, so a clean stop loses nothing and a crash at most
    // one window. `flush` logs its own failures and drops that batch.
    {
        let search = state.search().clone();
        let token = shutdown_token.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(crate::search::analytics::FLUSH_INTERVAL);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let _ = search.analytics().flush().await;
                    }
                    _ = token.cancelled() => {
                        let _ = search.analytics().flush().await;
                        break;
                    }
                }
            }
        });
    }

    // Spawn the resident plugin-queue runner (P11d / D-46), but only when it is
    // enabled in site_config at boot. The default deployment runs no queue
    // scheduler and keeps the external-cron cadence contract; enabling the
    // runner is an explicit, config-gated opt-in for sub-cron latency.
    {
        let cron = state.cron().clone();
        let runner_config = cron.queue_runner_config().await;
        if runner_config.enabled {
            let token = shutdown_token.clone();
            tokio::spawn(async move { cron.run_queue_runner(token).await });
            info!(
                poll_interval_secs = runner_config.poll_interval_secs,
                "Resident plugin-queue runner enabled (cron drain steps aside)"
            );
        }
    }

    // Create session layer
    let same_site = match config.cookie_same_site.as_str() {
        "lax" => SameSite::Lax,
        "none" => SameSite::None,
        _ => SameSite::Strict,
    };
    let session_layer = session::create_session_layer(&config.redis_url, same_site)
        .await
        .context("failed to create session layer")?;

    // Log plugin and content type info
    info!(
        plugins = state.plugin_runtime().plugin_count(),
        content_types = state.content_types().len(),
        "Plugins and content types loaded"
    );

    // Build CORS layer from config
    let cors = build_cors_layer(&config);

    // Build the inner router with all routes (no path alias — that's handled
    // by the fallback below so it runs BEFORE Axum route matching).
    // Say so, once, when a plugin declares a callback the kernel cannot reach.
    let plugin_menus = state.menu_registry().all().cloned().collect::<Vec<_>>();
    routes::plugin_api::warn_unreachable_callbacks(&plugin_menus);

    // Refuse, with a message naming the plugin, when one claims a path the
    // kernel serves and cannot yield. `Router::merge` panics on an overlapping
    // route, so before this check a plugin could take the process down at boot
    // with axum's message instead of an explanation of what to do about it.
    routes::sitemap::ensure_reserved_paths_unclaimed(&plugin_menus)?;

    let inner_router: Router<AppState> = Router::new()
        .merge(routes::front::router())
        .merge(routes::install::router())
        .merge(routes::auth::router())
        .merge(routes::user_delete::router())
        .merge(routes::admin::router())
        .merge(routes::password_reset::router())
        .merge(routes::webauthn::router())
        .merge(routes::sessions::router())
        .merge(routes::recovery::router())
        .merge(routes::health::router())
        .merge(routes::item::router())
        .merge(routes::gather::router())
        .merge(routes::gather_admin::router())
        .merge(routes::plugin_admin::router())
        .merge(routes::search::router())
        .merge(routes::cron::router())
        .merge(routes::file::router())
        .merge(routes::metrics::router())
        .merge(routes::batch::router())
        .merge(routes::api_token::router())
        .merge(routes::api_ai_assist::router())
        .merge(routes::api_chat::router())
        // The AI Assistant: its themed page and its four API routes.
        .merge(routes::assistant::router())
        .merge(routes::api_search::router())
        .merge(routes::api_v1::router())
        .merge(routes::tile_admin::router())
        .merge(routes::static_files::router())
        .merge(routes::sitemap::router())
        // Plugin-gated routes — runtime middleware returns 404 when disabled.
        .merge(routes::gated_plugin_routes(&state))
        // Dynamic gather route aliases from query display configs.
        .merge(routes::gather_routes::build_gather_route_router(
            &state.gather().list_queries(),
        ))
        // RSS feeds declared by gather query display configs.
        .merge(routes::feed::build_feed_router(&state.gather().list_queries()))
        // Plugin-served API routes from `tap_menu` entries whose handler_type
        // is `api` (G-NO-PLUGIN-HTTP). Entries that name a callback without
        // asking for `api` are logged: that combination registers nothing.
        .merge(routes::plugin_api::build_plugin_api_router(
            &state.menu_registry().all().cloned().collect::<Vec<_>>(),
        ));

    // Wrap the inner router with state so we can clone it for the fallback.
    let inner_with_state: Router = inner_router.clone().with_state(state.clone());
    let shared_router = Arc::new(inner_with_state);

    // Build the outer app with a fallback that handles path alias resolution.
    // In Axum 0.8, Router::layer() middleware runs AFTER route matching, so
    // URI rewriting in middleware cannot affect which route is matched.
    // The fallback receives all unmatched requests and forwards them to the
    // inner router after resolving any URL alias.
    let app = inner_router
        .fallback({
            let router = shared_router.clone();
            let app_state = state.clone();
            move |session: Session, request: axum::extract::Request| {
                let router = router.clone();
                let app_state = app_state.clone();
                async move {
                    crate::middleware::path_alias_fallback(app_state, session, router, request).await
                }
            }
        })
        // Middleware layers (last added = first executed in request flow):
        // TraceLayer → track_request_timing → security_headers → CORS → session → rate_limit(per-IP) →
        // bearer_auth → api_token → rate_limit(per-user) → install_check →
        // negotiate_language → redirect → routes
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::check_redirect,
        ))
        // FR-7b: maintain the per-user session index. Inside the session layer
        // so it can read the session the handler just wrote (a login, or a
        // cycle_id), which is exactly what it needs to observe.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::track_session,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::negotiate_language,
        ))
        // Maintenance mode: a 503 for everyone but a holder of the bypass
        // permission. After the installation check, inside the session layer.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::check_maintenance,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::check_installation,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::check_authenticated_rate_limit,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::authenticate_api_token,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::authenticate_bearer_token,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::check_rate_limit,
        ))
        // Resolve the trusted-proxy-gated client IP once (RATE-1), outer to the
        // rate-limit checks and all handlers so they share one vetted value.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::resolve_client_ip,
        ))
        // One error page (B84): every marked error, and every bare plain-text
        // one, rendered as the themed error page for a browser and as the
        // AppError body for anyone else. Inside the session layer, which the
        // page's navigation needs; outside everything else, so a rate-limit or
        // authentication refusal is rendered the same way.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::render_error_pages,
        ))
        // The theme each render uses: the admin theme for `/admin`, the
        // default otherwise (B108). Outside the error-page layer, so an error
        // page is drawn in the request's theme too.
        .layer(axum::middleware::from_fn(crate::middleware::scope_theme))
        .layer(session_layer)
        .layer(cors)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::inject_security_headers,
        ))
        // Request timing, outside every other layer so `Server-Timing` measures
        // the whole request rather than a part of it.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::track_request_timing,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    // Keep a reference to the DB pool for graceful shutdown cleanup.
    let db_pool = state.db().clone();

    // Start the server
    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context("failed to bind to address")?;

    info!(%addr, "Server listening");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server error")?;

    // Server has stopped accepting new connections.
    // Signal all background tasks to stop.
    info!("Signaling background tasks to shut down...");
    shutdown_token.cancel();

    // Give background tasks a moment to finish their current iteration.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Close external connections with a timeout.
    info!("Closing external connections...");
    let drain_result = tokio::time::timeout(config.shutdown_timeout, async {
        db_pool.close().await;
        info!("Database pool closed");
    })
    .await;

    if drain_result.is_err() {
        warn!(
            timeout_secs = config.shutdown_timeout.as_secs(),
            "Shutdown timeout reached, forcing exit"
        );
    }

    info!("Shutdown complete");
    Ok(())
}

/// Wait for a shutdown signal (SIGINT or SIGTERM).
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = ctrl_c => info!("Received SIGINT, starting graceful shutdown"),
                    _ = sigterm.recv() => info!("Received SIGTERM, starting graceful shutdown"),
                }
            }
            Err(e) => {
                warn!(error = %e, "Failed to install SIGTERM handler, using SIGINT only");
                ctrl_c.await.ok();
                info!("Received SIGINT, starting graceful shutdown");
            }
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
        info!("Received SIGINT, starting graceful shutdown");
    }
}

/// Run a plugin CLI command with a minimal context (pool only).
async fn run_plugin_command(action: PluginAction) -> Result<()> {
    // `New` needs no DB connection — handle it before pool setup.
    if let PluginAction::New { name } = action {
        let workspace_root = std::env::current_dir().context("failed to get current directory")?;
        return plugin::cli::cmd_plugin_new(&workspace_root, &name);
    }

    let config = Config::from_env().context("failed to load configuration")?;

    let pool = db::create_pool(&config)
        .await
        .context("failed to create database pool")?;

    // Run kernel migrations to ensure plugin_status table exists
    db::run_migrations(&pool)
        .await
        .context("failed to run migrations")?;

    match action {
        PluginAction::New { .. } => unreachable!("handled above"),
        PluginAction::List => {
            plugin::cli::cmd_plugin_list(&pool, &config.plugins_dirs).await?;
            return Ok(());
        }
        PluginAction::Install { name } => {
            plugin::cli::cmd_plugin_install(&pool, &config.plugins_dirs, &name).await?;
        }
        PluginAction::Migrate { name } => {
            plugin::cli::cmd_plugin_migrate(&pool, &config.plugins_dirs, name.as_deref()).await?;
        }
        PluginAction::Enable { name } => {
            plugin::cli::cmd_plugin_enable(&pool, &config.plugins_dirs, &name).await?;
        }
        PluginAction::Disable { name } => {
            plugin::cli::cmd_plugin_disable(&pool, &name).await?;
        }
    }

    // Install and migrate may write any table a plugin's migrations touch.
    // Which plugins run still waits for a restart.
    println!("{}", bump_from_cli(&config, &CacheKind::ALL).await);

    Ok(())
}

/// Run a config CLI command with a minimal context (pool only).
async fn run_config_command(action: ConfigAction) -> Result<()> {
    let config = Config::from_env().context("failed to load configuration")?;

    let pool = db::create_pool(&config)
        .await
        .context("failed to create database pool")?;

    db::run_migrations(&pool)
        .await
        .context("failed to run migrations")?;

    // `file` config entities carry bytes, which go to the site's file storage.
    let files = std::sync::Arc::new(crate::file::FileService::new(
        pool.clone(),
        std::sync::Arc::new(crate::file::LocalFileStorage::new(
            &config.uploads_dir,
            &config.files_url,
        )),
    ));
    // Plugin config tables are named in the manifests on disk.
    let plugins = config_storage::PluginConfigSet::discover(&config.plugins_dirs).await;
    let storage = config_storage::DirectConfigStorage::new(pool.clone())
        .with_files(files)
        .with_plugins(plugins);

    match action {
        ConfigAction::Export { dir, clean } => {
            let dir = std::path::PathBuf::from(dir);
            let result = config_storage::yaml::export_config(&storage, &pool, &dir, clean).await?;
            print_config_summary("Exported", &dir, &result.counts, &result.warnings);
        }
        ConfigAction::Import { dir, dry_run } => {
            let dir = std::path::PathBuf::from(dir);
            let result =
                config_storage::yaml::import_config(&storage, &pool, &dir, dry_run).await?;
            let verb = if dry_run { "Would import" } else { "Imported" };
            print_config_summary(verb, &dir, &result.counts, &result.warnings);
            if !dry_run {
                // Any cached kind may have changed, so every kind is bumped.
                println!("{}", bump_from_cli(&config, &CacheKind::ALL).await);
                if result.counts.contains_key("plugin_status") {
                    println!("{CLI_PLUGIN_RESTART_NOTE}");
                }
            }
        }
    }

    Ok(())
}

/// End every session of `user_id` through the session index in Redis, and say
/// what happened in a line fit for the terminal.
///
/// Used by the command line password reset, which runs in its own process
/// without an `AppState`. The site's session tracking refuses a session whose
/// index entry is gone, so removing the entries here ends the sessions in the
/// running site too.
async fn end_sessions_from_cli(config: &Config, user_id: uuid::Uuid) -> String {
    let registry = match redis::Client::open(config.redis_url.as_str()) {
        Ok(client) => services::session_registry::SessionRegistry::new(
            client,
            session::DEFAULT_SESSION_EXPIRY_HOURS * 3600,
        ),
        Err(e) => {
            return format!(
                "Existing sessions were NOT ended: the Redis URL is not usable ({e}). \
                 They end when they idle out, or from the user's sessions page."
            );
        }
    };
    match registry.revoke_all_except(user_id, None).await {
        Ok(revoked) => format!("Ended {} existing session(s).", revoked.len()),
        Err(e) => format!(
            "Existing sessions were NOT ended: Redis could not be reached ({e:#}). \
             They end when they idle out, or from the user's sessions page."
        ),
    }
}

/// Run a user CLI command with a minimal context (pool only).
async fn run_user_command(action: UserAction) -> Result<()> {
    let config = Config::from_env().context("failed to load configuration")?;

    let pool = db::create_pool(&config)
        .await
        .context("failed to create database pool")?;

    db::run_migrations(&pool)
        .await
        .context("failed to run migrations")?;

    match action {
        UserAction::ResetPassword { username, password } => {
            let password = match password {
                Some(p) => p,
                None => {
                    eprint!("New password: ");
                    let mut buf = String::new();
                    std::io::stdin()
                        .read_line(&mut buf)
                        .context("failed to read password from stdin")?;
                    buf.trim().to_string()
                }
            };

            if password.len() < 12 {
                anyhow::bail!("Password must be at least 12 characters.");
            }

            let user = models::User::find_by_name(&pool, &username)
                .await?
                .context(format!("User '{username}' not found."))?;

            let updated = models::User::update_password(&pool, user.id, &password).await?;
            if updated {
                println!("Password updated for user '{username}'.");
            } else {
                anyhow::bail!("Failed to update password for user '{username}'.");
            }

            // A reset ends the account's existing sessions, as a reset through
            // the site does. This process has no running site to ask, so it
            // goes to the same Redis-backed session index directly. The
            // password is already changed either way, so a Redis that cannot
            // be reached is reported rather than turned into a failure.
            println!("{}", end_sessions_from_cli(&config, user.id).await);
            println!("{}", bump_from_cli(&config, &[CacheKind::Users]).await);
        }
        UserAction::RoleAdd { username, role } => {
            let (user, role) = find_user_and_role(&pool, &username, &role).await?;
            // Idempotent: `assign_to_user` upserts, so re-running is not an
            // error, and saying which it was is more use than a bare "done".
            let already = models::Role::get_user_roles(&pool, user.id)
                .await?
                .into_iter()
                .any(|r| r.id == role.id);
            models::Role::assign_to_user(&pool, user.id, role.id).await?;
            if already {
                println!("User '{username}' already had role '{}'.", role.name);
            } else {
                println!("User '{username}' now has role '{}'.", role.name);
                println!(
                    "{}",
                    bump_from_cli(&config, &[CacheKind::Permissions]).await
                );
            }
        }
        UserAction::RoleRemove { username, role } => {
            let (user, role) = find_user_and_role(&pool, &username, &role).await?;
            models::Role::remove_from_user(&pool, user.id, role.id).await?;
            println!("User '{username}' no longer has role '{}'.", role.name);
            println!(
                "{}",
                bump_from_cli(&config, &[CacheKind::Permissions]).await
            );
        }
        UserAction::Roles { username } => {
            let user = models::User::find_by_name(&pool, &username)
                .await?
                .context(format!("User '{username}' not found."))?;
            let roles = models::Role::get_user_roles(&pool, user.id).await?;
            if roles.is_empty() {
                println!("User '{username}' holds no roles.");
            } else {
                println!("User '{username}' holds {} role(s):", roles.len());
                for role in roles {
                    println!("  {}", role.name);
                }
            }
        }
    }

    Ok(())
}

/// Resolve a username and a role name, or say which one was not found.
///
/// Both lookups are by name rather than id, because a name is what an operator
/// has in front of them. Naming the missing one matters: "not found" on its own
/// leaves them checking both.
async fn find_user_and_role(
    pool: &sqlx::PgPool,
    username: &str,
    role: &str,
) -> Result<(models::User, models::Role)> {
    let user = models::User::find_by_name(pool, username)
        .await?
        .context(format!("User '{username}' not found."))?;
    let role = models::Role::find_by_name(pool, role)
        .await?
        .context(format!("Role '{role}' not found."))?;
    Ok((user, role))
}

/// Which plugins run is read at boot, on every server, so an imported plugin
/// status waits for a restart; everything else an import writes does not.
const CLI_PLUGIN_RESTART_NOTE: &str = "Plugin enable and disable take effect when the servers restart (all of them, if more than one).";

/// Tell every running server to drop its caches of `kinds` (`cache::generation`),
/// and say what happened in a line fit for the terminal.
///
/// A command runs in its own process, with no server cache of its own to
/// clear; the bump is how the servers hear about the write.
async fn bump_from_cli(config: &Config, kinds: &[CacheKind]) -> String {
    let bumped = match redis::Client::open(config.redis_url.as_str()) {
        Ok(client) => cache::generation::bump_now(&client, kinds).await,
        Err(e) => Err(e.into()),
    };
    match bumped {
        Ok(()) => "Running servers drop their cached copies within a second.".to_string(),
        Err(e) => format!(
            "Running servers were NOT told: Redis could not be reached ({e:#}). \
             They see this change when their caches expire."
        ),
    }
}

fn print_config_summary(
    verb: &str,
    dir: &std::path::Path,
    counts: &std::collections::BTreeMap<String, usize>,
    warnings: &[String],
) {
    let total: usize = counts.values().sum();
    println!("{verb} {total} config entities ({})", dir.display());
    for (entity_type, count) in counts {
        println!("  {entity_type}: {count}");
    }
    if !warnings.is_empty() {
        println!("{} warning(s):", warnings.len());
        for warning in warnings {
            println!("  warning: {warning}");
        }
    }
}

fn build_cors_layer(config: &Config) -> CorsLayer {
    let methods = [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::DELETE,
        Method::OPTIONS,
    ];

    if config.cors_allowed_origins.len() == 1 && config.cors_allowed_origins[0] == "*" {
        CorsLayer::new()
            .allow_origin(tower_http::cors::Any)
            .allow_methods(methods)
            .allow_headers(tower_http::cors::Any)
    } else if config.cors_allowed_origins.is_empty() {
        // No CORS origins configured — block cross-origin requests.
        // Use a restrictive layer that allows nothing from other origins.
        CorsLayer::new().allow_methods(methods).allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
        ])
    } else {
        let origins: Vec<HeaderValue> = config
            .cors_allowed_origins
            .iter()
            .filter_map(|o| match o.parse::<HeaderValue>() {
                Ok(v) => Some(v),
                Err(_) => {
                    warn!(origin = %o, "ignoring unparseable CORS origin");
                    None
                }
            })
            .collect();

        // Specific origins: enable credentials with explicit headers
        // (tower-http disallows credentials + wildcard headers).
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(methods)
            .allow_headers([
                axum::http::header::CONTENT_TYPE,
                axum::http::header::AUTHORIZATION,
            ])
            .allow_credentials(true)
    }
}

fn init_tracing() -> system_log::SystemLogReceiver {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=debug,sqlx=warn"));
    let (log_layer, log_receiver) = system_log::SystemLogLayer::new();

    // The site log takes its own level decision (warn and error, and the
    // allowlisted info events), so it sits beside the `RUST_LOG` filter rather
    // than under it: a quiet `RUST_LOG=error` must not empty the admin's log.
    // Its own `INFO` filter keeps debug and trace callsites disabled.
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(filter))
        .with(log_layer.with_filter(tracing_subscriber::filter::LevelFilter::INFO))
        .init();
    log_receiver
}
