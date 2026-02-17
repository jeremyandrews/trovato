//! Trovato CMS Kernel
//!
//! HTTP server, plugin runtime, and core services.

mod batch;
mod cache;
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
mod permissions;
mod plugin;
mod routes;
mod search;
mod session;
mod stage;
mod state;
mod tap;
mod theme;

use std::net::SocketAddr;

use anyhow::{Context, Result};
use axum::Router;
use axum::http::{HeaderValue, Method};
use clap::{Parser, Subcommand};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tower_sessions::cookie::SameSite;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::Config;
use crate::state::AppState;

#[derive(Parser)]
#[command(name = "trovato", about = "Trovato CMS")]
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
enum PluginAction {
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

    // Initialize tracing
    init_tracing();

    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::Serve) => run_server().await,
        Some(Commands::Plugin { action }) => run_plugin_command(action).await,
        Some(Commands::Config { action }) => run_config_command(action).await,
    }
}

/// Run the HTTP server (original startup path).
async fn run_server() -> Result<()> {
    info!("Starting Trovato CMS kernel");

    // Load configuration from environment
    let config = Config::from_env().context("failed to load configuration")?;
    info!(port = config.port, "Configuration loaded");

    // Initialize application state (database connections, etc.)
    let state = AppState::new(&config)
        .await
        .context("failed to initialize application state")?;

    info!("Database and Redis connections established");

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

    // Build the router
    let app = Router::new()
        .merge(routes::front::router())
        .merge(routes::install::router())
        .merge(routes::auth::router())
        .merge(routes::admin::router())
        .merge(routes::password_reset::router())
        .merge(routes::health::router())
        .merge(routes::item::router())
        .merge(routes::category::router())
        .merge(routes::comment::router())
        .merge(routes::gather::router())
        .merge(routes::search::router())
        .merge(routes::cron::router())
        .merge(routes::file::router())
        .merge(routes::metrics::router())
        .merge(routes::batch::router())
        .merge(routes::api_token::router())
        .merge(routes::static_files::router())
        // Middleware layers (last added = first executed in request flow):
        // TraceLayer → session → CORS → api_token → install_check → negotiate_language → path_alias → routes
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::resolve_path_alias,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::negotiate_language,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::check_installation,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::authenticate_api_token,
        ))
        .layer(session_layer)
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    // Start the server
    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context("failed to bind to address")?;

    info!(%addr, "Server listening");

    axum::serve(listener, app).await.context("server error")?;

    Ok(())
}

/// Run a plugin CLI command with a minimal context (pool only).
async fn run_plugin_command(action: PluginAction) -> Result<()> {
    let config = Config::from_env().context("failed to load configuration")?;

    let pool = db::create_pool(&config)
        .await
        .context("failed to create database pool")?;

    // Run kernel migrations to ensure plugin_status table exists
    db::run_migrations(&pool)
        .await
        .context("failed to run migrations")?;

    match action {
        PluginAction::List => {
            plugin::cli::cmd_plugin_list(&pool, &config.plugins_dir).await?;
        }
        PluginAction::Install { name } => {
            plugin::cli::cmd_plugin_install(&pool, &config.plugins_dir, &name).await?;
        }
        PluginAction::Migrate { name } => {
            plugin::cli::cmd_plugin_migrate(&pool, &config.plugins_dir, name.as_deref()).await?;
        }
        PluginAction::Enable { name } => {
            plugin::cli::cmd_plugin_enable(&pool, &name).await?;
        }
        PluginAction::Disable { name } => {
            plugin::cli::cmd_plugin_disable(&pool, &name).await?;
        }
    }

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

    let storage = config_storage::DirectConfigStorage::new(pool.clone());

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
        }
    }

    Ok(())
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

        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(methods)
            .allow_headers(tower_http::cors::Any)
            .allow_credentials(true)
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=debug,sqlx=warn"));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}
