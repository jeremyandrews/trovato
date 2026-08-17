//! Configuration loaded from environment variables.

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

/// Default cache TTL in seconds (1 minute).
const DEFAULT_CACHE_TTL: u64 = 60;

/// Cache configuration loaded from `CACHE_TTL*` environment variables.
///
/// Each per-cache TTL defaults to `CACHE_TTL` if its own variable is unset.
/// `CACHE_TTL` itself defaults to 60 seconds.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Content type registry reload interval.
    pub ttl_content_types: Duration,
    /// Gather query registry reload interval.
    pub ttl_gather_queries: Duration,
    /// Permission cache entry TTL.
    pub ttl_permissions: Duration,
    /// User cache entry TTL.
    pub ttl_users: Duration,
    /// Item cache entry TTL.
    pub ttl_items: Duration,
    /// Category cache entry TTL.
    pub ttl_categories: Duration,
}

impl CacheConfig {
    /// Load cache configuration from environment variables.
    ///
    /// Resolution: each `CACHE_TTL_<NAME>` falls back to `CACHE_TTL`,
    /// which itself defaults to 60 seconds.
    pub fn from_env() -> Self {
        let global = Self::parse_env_u64("CACHE_TTL").unwrap_or(DEFAULT_CACHE_TTL);
        Self {
            ttl_content_types: Duration::from_secs(
                Self::parse_env_u64("CACHE_TTL_CONTENT_TYPES").unwrap_or(global),
            ),
            ttl_gather_queries: Duration::from_secs(
                Self::parse_env_u64("CACHE_TTL_GATHER_QUERIES").unwrap_or(global),
            ),
            ttl_permissions: Duration::from_secs(
                Self::parse_env_u64("CACHE_TTL_PERMISSIONS").unwrap_or(global),
            ),
            ttl_users: Duration::from_secs(Self::parse_env_u64("CACHE_TTL_USERS").unwrap_or(300)),
            ttl_items: Duration::from_secs(Self::parse_env_u64("CACHE_TTL_ITEMS").unwrap_or(300)),
            ttl_categories: Duration::from_secs(
                Self::parse_env_u64("CACHE_TTL_CATEGORIES").unwrap_or(300),
            ),
        }
    }

    /// Parse an environment variable as `u64`, returning `None` if unset or invalid.
    fn parse_env_u64(name: &str) -> Option<u64> {
        env::var(name).ok().and_then(|v| v.parse().ok())
    }
}

/// How [`PluginConfig::from_lookup`](crate::plugin::PluginConfig::from_lookup)
/// asks for a setting: by name, `None` when not configured.
///
/// A trait object rather than a generic, so the parse helpers below can be
/// ordinary functions instead of one monomorphized per call site.
type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Parse a looked-up setting via [`std::str::FromStr`], falling back to
/// `default` when it is absent or unparseable.
fn parse_or<T: std::str::FromStr>(lookup: Lookup<'_>, name: &str, default: T) -> T {
    lookup(name)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// Parse a boolean-ish looked-up setting. Truthy: `1`, `true`, `yes`, `on`
/// (case-insensitive); anything else present is `false`; absent is `default`.
fn parse_bool_or(lookup: Lookup<'_>, name: &str, default: bool) -> bool {
    lookup(name).map_or(default, |v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

impl crate::plugin::PluginConfig {
    /// Load the plugin runtime configuration from environment variables.
    ///
    /// Pooling-allocator sizing:
    /// - `PLUGIN_MAX_INSTANCES` (default 1000) — total pooled instances.
    /// - `PLUGIN_MAX_MEMORY_PAGES` (default 1024 = 64 MiB) — per-instance slab.
    ///
    /// Per-`Store` limiter ([`ResourceLimits`](crate::plugin::limits::ResourceLimits),
    /// WASM-4):
    /// - `PLUGIN_LIMIT_MEMORY_BYTES` (default 64 MiB) — linear-memory cap.
    /// - `PLUGIN_LIMIT_TABLE_ELEMENTS` (default 10000) — table element cap.
    /// - `PLUGIN_LIMIT_MEMORIES` (default 1) — memories per `Store`.
    /// - `PLUGIN_LIMIT_TABLES` (default 1) — tables per `Store`.
    /// - `PLUGIN_LIMIT_INSTANCES` (default 1) — instances per `Store`.
    /// - `PLUGIN_ENABLE_FUEL` (default false) — opt-in fuel metering.
    /// - `PLUGIN_FUEL_LIMIT` (default 10_000_000_000) — per-`Store` fuel budget.
    ///
    /// Every variable falls back to its documented default when unset or
    /// unparseable. The pool slab and the limiter memory cap are kept coherent at
    /// engine creation (the slab is raised to never fall below the limiter cap).
    ///
    /// This is a one-line edge over `from_lookup`, which holds all of the
    /// resolution logic and the variable names. The split is what lets the
    /// resolution be tested without mutating the process environment.
    pub fn from_env() -> Self {
        Self::from_lookup(|name| env::var(name).ok())
    }

    /// Resolve the plugin runtime configuration from an arbitrary settings
    /// lookup, as documented on [`Self::from_env`].
    ///
    /// The lookup, not the process environment, is the only source of values, so
    /// a test can drive every documented variable — and every documented default
    /// — from an explicit map with nothing global involved.
    pub(crate) fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        use crate::plugin::limits::ResourceLimits;
        let cfg_defaults = Self::default();
        let lim_defaults = ResourceLimits::default();
        let lookup: Lookup<'_> = &lookup;
        Self {
            max_instances: parse_or(lookup, "PLUGIN_MAX_INSTANCES", cfg_defaults.max_instances),
            max_memory_pages: parse_or(
                lookup,
                "PLUGIN_MAX_MEMORY_PAGES",
                cfg_defaults.max_memory_pages,
            ),
            limits: ResourceLimits {
                max_memory_bytes: parse_or(
                    lookup,
                    "PLUGIN_LIMIT_MEMORY_BYTES",
                    lim_defaults.max_memory_bytes,
                ),
                max_table_elements: parse_or(
                    lookup,
                    "PLUGIN_LIMIT_TABLE_ELEMENTS",
                    lim_defaults.max_table_elements,
                ),
                max_memories: parse_or(lookup, "PLUGIN_LIMIT_MEMORIES", lim_defaults.max_memories),
                max_tables: parse_or(lookup, "PLUGIN_LIMIT_TABLES", lim_defaults.max_tables),
                max_instances: parse_or(
                    lookup,
                    "PLUGIN_LIMIT_INSTANCES",
                    lim_defaults.max_instances,
                ),
                enable_fuel: parse_bool_or(lookup, "PLUGIN_ENABLE_FUEL", lim_defaults.enable_fuel),
                fuel_limit: parse_or(lookup, "PLUGIN_FUEL_LIMIT", lim_defaults.fuel_limit),
            },
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use crate::plugin::PluginConfig;
    use std::collections::HashMap;

    /// Drive [`PluginConfig::from_lookup`] from an explicit settings map.
    ///
    /// Nothing here touches the process environment: the variable *names* still
    /// live in `from_lookup`, so a name typo fails these tests, while the
    /// resolution runs against values this test owns outright.
    fn from_map(pairs: &[(&str, &str)]) -> PluginConfig {
        let settings: HashMap<&str, &str> = pairs.iter().copied().collect();
        PluginConfig::from_lookup(|name| settings.get(name).map(|v| (*v).to_string()))
    }

    /// Every documented variable steers its field, under its documented name.
    #[test]
    fn plugin_config_lookup_overrides_every_documented_setting() {
        let config = from_map(&[
            ("PLUGIN_MAX_INSTANCES", "42"),
            ("PLUGIN_MAX_MEMORY_PAGES", "128"),
            ("PLUGIN_LIMIT_MEMORY_BYTES", "1048576"),
            ("PLUGIN_LIMIT_TABLE_ELEMENTS", "7"),
            ("PLUGIN_LIMIT_MEMORIES", "4"),
            ("PLUGIN_LIMIT_TABLES", "5"),
            ("PLUGIN_LIMIT_INSTANCES", "3"),
            ("PLUGIN_ENABLE_FUEL", "true"),
            ("PLUGIN_FUEL_LIMIT", "555"),
        ]);
        assert_eq!(config.max_instances, 42);
        assert_eq!(config.max_memory_pages, 128);
        assert_eq!(config.limits.max_memory_bytes, 1_048_576);
        assert_eq!(config.limits.max_table_elements, 7);
        assert_eq!(config.limits.max_memories, 4);
        assert_eq!(config.limits.max_tables, 5);
        assert_eq!(config.limits.max_instances, 3);
        assert!(config.limits.enable_fuel);
        assert_eq!(config.limits.fuel_limit, 555);
    }

    /// Nothing configured yields the documented defaults.
    ///
    /// "Nothing configured" is asserted, not hoped for: the old shape of this
    /// test read the real environment, so a CI runner or developer shell that
    /// exported any `PLUGIN_*` variable failed it spuriously.
    #[test]
    fn plugin_config_defaults_when_nothing_is_configured() {
        let config = from_map(&[]);
        assert_eq!(config.max_instances, 1000);
        assert_eq!(config.max_memory_pages, 1024);
        assert_eq!(config.limits.max_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(config.limits.max_table_elements, 10_000);
        assert_eq!(config.limits.max_memories, 1);
        assert_eq!(config.limits.max_tables, 1);
        assert_eq!(config.limits.max_instances, 1);
        assert!(!config.limits.enable_fuel);
        assert_eq!(config.limits.fuel_limit, 10_000_000_000);
    }

    /// The documented "unset *or unparseable*" fallback, which the old
    /// env-mutating test never covered.
    #[test]
    fn plugin_config_unparseable_values_fall_back_to_defaults() {
        let config = from_map(&[
            ("PLUGIN_MAX_INSTANCES", "many"),
            ("PLUGIN_MAX_MEMORY_PAGES", ""),
            ("PLUGIN_LIMIT_MEMORY_BYTES", "64 MiB"),
            ("PLUGIN_FUEL_LIMIT", "-1"),
        ]);
        assert_eq!(config.max_instances, 1000);
        assert_eq!(config.max_memory_pages, 1024);
        assert_eq!(config.limits.max_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(config.limits.fuel_limit, 10_000_000_000);
    }

    /// Surrounding whitespace is tolerated, and only the documented spellings
    /// are truthy — a present-but-unrecognized value is `false`, not the
    /// default, which is what `parse_bool_or` promises.
    #[test]
    fn plugin_config_boolean_spellings() {
        for truthy in ["1", "true", "TRUE", "yes", "on", " on "] {
            assert!(
                from_map(&[("PLUGIN_ENABLE_FUEL", truthy)])
                    .limits
                    .enable_fuel,
                "{truthy:?} should enable fuel"
            );
        }
        for falsy in ["0", "false", "off", "no", "maybe", ""] {
            assert!(
                !from_map(&[("PLUGIN_ENABLE_FUEL", falsy)])
                    .limits
                    .enable_fuel,
                "{falsy:?} should not enable fuel"
            );
        }
        assert_eq!(
            from_map(&[("PLUGIN_MAX_INSTANCES", " 42 ")]).max_instances,
            42
        );
    }
}

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

    /// Plugin search path (default: a single `./plugins` entry).
    ///
    /// Multiple directories let an application ship its plugins from its own
    /// repository instead of copying build artifacts into the kernel tree,
    /// which is what an external app like Ritrovo needs in order to install
    /// without a Trovato checkout. Later directories win on a name collision.
    pub plugins_dirs: Vec<PathBuf>,

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

    /// Maximum allowed `per_page` for Gather queries (default: 100).
    ///
    /// Runtime request parameters are clamped to this value. Gather
    /// definition `items_per_page` is not clamped — only the runtime
    /// request parameter is.
    pub gather_max_page_size: u32,

    /// Language negotiation methods, in priority order.
    ///
    /// Comma-separated list from `LANGUAGE_NEGOTIATION_METHODS` env var.
    /// Supported methods: `url_prefix`, `accept_header`, `cookie`.
    /// Default: `url_prefix,accept_header`.
    /// Single-language sites skip negotiation entirely regardless of this setting.
    pub language_negotiation_methods: Vec<String>,
}

/// Read an environment variable as a platform search path.
///
/// Splits on the platform path separator (`:` on unix, `;` on windows) via
/// `env::split_paths`, so a plain single-directory value parses to a
/// one-element list and every pre-existing deployment keeps its old behaviour.
/// Empty segments are dropped, which is what makes a trailing or doubled
/// separator harmless rather than a silent "current directory" entry.
///
/// Falls back to `default` when the variable is unset, or when it is set to
/// something that contains no usable segment at all.
///
/// A one-line edge over [`split_search_path_value`], which holds the parsing so
/// that it can be tested without touching the process environment.
pub(crate) fn split_search_path(var: &str, default: &str) -> Vec<PathBuf> {
    split_search_path_value(env::var_os(var).as_deref(), default)
}

/// Parse an already-read search-path value, as documented on
/// [`split_search_path`]. `None` is "the variable is not set".
pub(crate) fn split_search_path_value(
    value: Option<&std::ffi::OsStr>,
    default: &str,
) -> Vec<PathBuf> {
    let dirs: Vec<PathBuf> = match value {
        Some(value) => env::split_paths(value)
            .filter(|p| !p.as_os_str().is_empty())
            .collect(),
        None => Vec::new(),
    };

    if dirs.is_empty() {
        vec![PathBuf::from(default)]
    } else {
        dirs
    }
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

        // PLUGINS_DIR is a search path, not a single directory. It is split on
        // the platform path separator (`:` on unix), so the historical
        // single-directory value keeps parsing to a one-element list and
        // existing deployments are unaffected.
        let plugins_dirs = split_search_path("PLUGINS_DIR", "./plugins");

        let uploads_dir = env::var("UPLOADS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./uploads"));

        let files_url = env::var("FILES_URL").unwrap_or_else(|_| "/files".to_string());

        let cors_allowed_origins = env::var("CORS_ALLOWED_ORIGINS")
            .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_else(|_| Vec::new());

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

        let gather_max_page_size = env::var("GATHER_MAX_PAGE_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        let language_negotiation_methods = env::var("LANGUAGE_NEGOTIATION_METHODS")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_else(|_| vec!["url_prefix".to_string(), "accept_header".to_string()]);

        Ok(Self {
            port,
            database_url,
            redis_url,
            database_max_connections,
            plugins_dirs,
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
            gather_max_page_size,
            language_negotiation_methods,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod search_path_tests {
    use super::split_search_path_value;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    // --- Search-path parsing ---

    /// Parse a search-path value the way the env edge would, from an explicit
    /// string instead of from a variable nobody else in the binary can see
    /// change.
    fn parse(value: &str) -> Vec<PathBuf> {
        split_search_path_value(Some(OsStr::new(value)), "./plugins")
    }

    /// `split_search_path` is the compatibility hinge for PLUGINS_DIR,
    /// TEMPLATES_DIR and STATIC_DIR: all three used to be single directories, so
    /// a plain value must still parse to exactly one entry.
    ///
    /// Separators are written literally, matching the unix hosts this project
    /// builds and ships on.
    #[test]
    fn search_path_parsing() {
        // Unset falls back to the documented default.
        assert_eq!(
            split_search_path_value(None, "./plugins"),
            vec![PathBuf::from("./plugins")]
        );

        // A single directory stays a single entry (the pre-existing behaviour).
        assert_eq!(parse("/srv/plugins"), vec![PathBuf::from("/srv/plugins")]);

        // Multiple entries split in order, which is what lets an app append its
        // own directory after the kernel's.
        assert_eq!(
            parse("/srv/plugins:/opt/app/plugins"),
            vec![
                PathBuf::from("/srv/plugins"),
                PathBuf::from("/opt/app/plugins"),
            ]
        );

        // Empty segments are dropped rather than becoming a silent "current
        // directory" entry, so a trailing or doubled separator is harmless.
        assert_eq!(parse("/srv/plugins::"), vec![PathBuf::from("/srv/plugins")]);
        assert_eq!(parse(":/srv/plugins"), vec![PathBuf::from("/srv/plugins")]);

        // A value with nothing usable in it falls back to the default.
        assert_eq!(parse(":"), vec![PathBuf::from("./plugins")]);
        assert_eq!(parse(""), vec![PathBuf::from("./plugins")]);
    }
}
