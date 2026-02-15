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
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::Config;
use crate::state::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    // Initialize tracing
    init_tracing();

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
    let session_layer = session::create_session_layer(&config.redis_url)
        .await
        .context("failed to create session layer")?;

    // Log plugin and content type info
    info!(
        plugins = state.plugin_runtime().plugin_count(),
        content_types = state.content_types().len(),
        "Plugins and content types loaded"
    );

    // Build the router
    let app = Router::new()
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
        .merge(routes::static_files::router())
        // Path alias middleware runs first (last added = first executed)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::resolve_path_alias,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::check_installation,
        ))
        .layer(session_layer)
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

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=debug,sqlx=warn"));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}
