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

    /// Path to uploads directory (default: ./uploads).
    pub uploads_dir: PathBuf,

    /// Base URL for serving uploaded files (default: /files).
    pub files_url: String,

    /// CORS allowed origins (comma-separated, default: "*").
    pub cors_allowed_origins: Vec<String>,

    /// Cookie SameSite policy: "strict", "lax", or "none" (default: "strict").
    pub cookie_same_site: String,

    /// Plugin names to force-disable on first install (from DISABLED_PLUGINS env var).
    pub disabled_plugins: Vec<String>,

    /// SMTP host for email delivery. When None, email is disabled.
    pub smtp_host: Option<String>,

    /// SMTP port (default: 587).
    pub smtp_port: u16,

    /// SMTP username for authentication.
    pub smtp_username: Option<String>,

    /// SMTP password for authentication.
    pub smtp_password: Option<String>,

    /// SMTP encryption mode: "starttls" (default), "tls", or "none".
    pub smtp_encryption: String,

    /// From address for outgoing email.
    pub smtp_from_email: String,

    /// Public site URL for constructing links in emails.
    pub site_url: String,
}

impl Config {
    /// Load configuration from environment variables.
    pub fn from_env() -> Result<Self> {
        let port = env::var("PORT")
            .unwrap_or_else(|_| "3000".to_string())
            .parse()
            .context("PORT must be a valid u16")?;

        let database_url =
            env::var("DATABASE_URL").context("DATABASE_URL environment variable is required")?;

        let redis_url =
            env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

        let database_max_connections = env::var("DATABASE_MAX_CONNECTIONS")
            .unwrap_or_else(|_| "10".to_string())
            .parse()
            .context("DATABASE_MAX_CONNECTIONS must be a valid u32")?;

        let plugins_dir = env::var("PLUGINS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./plugins"));

        let uploads_dir = env::var("UPLOADS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./uploads"));

        let files_url = env::var("FILES_URL").unwrap_or_else(|_| "/files".to_string());

        let cors_allowed_origins = env::var("CORS_ALLOWED_ORIGINS")
            .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_else(|_| vec!["*".to_string()]);

        let cookie_same_site = env::var("COOKIE_SAME_SITE")
            .unwrap_or_else(|_| "strict".to_string())
            .to_lowercase();

        let disabled_plugins = env::var("DISABLED_PLUGINS")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let smtp_host = env::var("SMTP_HOST").ok();

        let smtp_port = env::var("SMTP_PORT")
            .unwrap_or_else(|_| "587".to_string())
            .parse()
            .context("SMTP_PORT must be a valid u16")?;

        let smtp_username = env::var("SMTP_USERNAME").ok();
        let smtp_password = env::var("SMTP_PASSWORD").ok();

        let smtp_encryption = env::var("SMTP_ENCRYPTION")
            .unwrap_or_else(|_| "starttls".to_string())
            .to_lowercase();

        let smtp_from_email =
            env::var("SMTP_FROM_EMAIL").unwrap_or_else(|_| "noreply@localhost".to_string());

        let site_url = env::var("SITE_URL").unwrap_or_else(|_| format!("http://localhost:{port}"));

        Ok(Self {
            port,
            database_url,
            redis_url,
            database_max_connections,
            plugins_dir,
            uploads_dir,
            files_url,
            cors_allowed_origins,
            cookie_same_site,
            disabled_plugins,
            smtp_host,
            smtp_port,
            smtp_username,
            smtp_password,
            smtp_encryption,
            smtp_from_email,
            site_url,
        })
    }
}
