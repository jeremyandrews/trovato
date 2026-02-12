//! Configuration loaded from environment variables.

use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// Application configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// HTTP server port (default: 3000).
    pub port: u16,

    /// PostgreSQL connection URL.
    pub database_url: String,

    /// Redis connection URL.
    pub redis_url: String,

    /// Maximum database connections in pool (default: 10).
    pub database_max_connections: u32,

    /// Path to plugins directory (default: ./plugins).
    pub plugins_dir: PathBuf,
}

impl Config {
    /// Load configuration from environment variables.
    pub fn from_env() -> Result<Self> {
        let port = env::var("PORT")
            .unwrap_or_else(|_| "3000".to_string())
            .parse()
            .context("PORT must be a valid u16")?;

        let database_url = env::var("DATABASE_URL")
            .context("DATABASE_URL environment variable is required")?;

        let redis_url = env::var("REDIS_URL")
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

        let database_max_connections = env::var("DATABASE_MAX_CONNECTIONS")
            .unwrap_or_else(|_| "10".to_string())
            .parse()
            .context("DATABASE_MAX_CONNECTIONS must be a valid u32")?;

        let plugins_dir = env::var("PLUGINS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./plugins"));

        Ok(Self {
            port,
            database_url,
            redis_url,
            database_max_connections,
            plugins_dir,
        })
    }
}
