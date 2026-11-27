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

/// How a `from_lookup` constructor asks for a setting: by name, `None` when not
/// configured.
///
/// Every group of settings in the kernel resolves through one of these rather
/// than reading the environment directly, which is what lets the resolution be
/// tested from an explicit map. [`env_lookup`] is the one adapter that closes
/// over the real environment, and `Config::from_env` is the only place it is
/// used.
///
/// A trait object rather than a generic, so the parse helpers below can be
/// ordinary functions instead of one monomorphized per call site.
pub(crate) type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The lookup that reads the real process environment.
///
/// Deliberately the only one: `Config::from_env` is the single boundary at which
/// this process's environment is consulted, so nothing downstream of it has to
/// re-read a variable, and no test has to mutate one to steer behaviour.
fn env_lookup(name: &str) -> Option<String> {
    env::var(name).ok()
}

/// Parse a looked-up setting via [`std::str::FromStr`], falling back to
/// `default` when it is absent or unparseable.
pub(crate) fn parse_or<T: std::str::FromStr>(lookup: Lookup<'_>, name: &str, default: T) -> T {
    lookup(name)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// The shortest `HEALTH_TOKEN` accepted: it is checked on requests that no
/// rate limit slows down.
const MIN_HEALTH_TOKEN_LEN: usize = 32;

/// Parse a boolean-ish looked-up setting. Truthy: `1`, `true`, `yes`, `on`
/// (case-insensitive); anything else present is `false`; absent is `default`.
pub(crate) fn parse_bool_or(lookup: Lookup<'_>, name: &str, default: bool) -> bool {
    lookup(name).map_or(default, |v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Parse a looked-up setting that must be positive to be meaningful, falling
/// back to `default` when it is absent, unparseable, or zero.
///
/// Zero is rejected rather than honoured for every setting that uses this: a
/// zero over-fetch factor, scan cap, or round cap would make the loop it bounds
/// either infinite or unable to return a single row.
pub(crate) fn parse_positive_or(lookup: Lookup<'_>, name: &str, default: u32) -> u32 {
    lookup(name)
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
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
    /// - `PLUGIN_BACKGROUND_TAP_DEADLINE_SECS` (default 150) — epoch budget for
    ///   a background tap, and so the longest a single queue job may burn CPU.
    ///
    /// Every variable falls back to its documented default when unset or
    /// unparseable. The pool slab and the limiter memory cap are kept coherent at
    /// engine creation (the slab is raised to never fall below the limiter cap).
    ///
    /// This is a one-line edge over `from_lookup`, which holds all of the
    /// resolution logic and the variable names. The split is what lets the
    /// resolution be tested without mutating the process environment.
    pub fn from_env() -> Self {
        Self::from_lookup(env_lookup)
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
                background_tap_epoch_deadline_secs: parse_or(
                    lookup,
                    "PLUGIN_BACKGROUND_TAP_DEADLINE_SECS",
                    lim_defaults.background_tap_epoch_deadline_secs,
                ),
                request_wallclock_ceiling_secs: parse_or(
                    lookup,
                    "PLUGIN_REQUEST_WALLCLOCK_SECS",
                    lim_defaults.request_wallclock_ceiling_secs,
                ),
                background_wallclock_ceiling_secs: parse_or(
                    lookup,
                    "PLUGIN_BACKGROUND_WALLCLOCK_SECS",
                    lim_defaults.background_wallclock_ceiling_secs,
                ),
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

/// The configuration that request handling reads.
///
/// Everything here used to be read from the process environment at the point of
/// use — per request for the CSP headers, the cron key and the tenant strategy,
/// per query for the slow-request threshold, per served file for the static
/// search path. Reading the environment lazily and repeatedly meant the only way
/// to steer any of it was to mutate a process-global, which is what forced the
/// test suite into `set_var`. Resolved once by [`Config::from_env`] and carried
/// on `AppState`, these are ordinary inputs a caller can set.
///
/// Kept separate from [`Config`] rather than putting `Config` itself on
/// `AppState`: `Config` holds `database_url`, `smtp_password` and `jwt_secret`,
/// and `AppState` is handed to every request handler in the process.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Static-asset search path. A later directory wins a collision.
    pub static_dirs: Vec<PathBuf>,

    /// Where the Pagefind index is written and served from (`PAGEFIND_OUTPUT_DIR`,
    /// default `pagefind` under the first static directory). Its URL is
    /// `/static/pagefind/` wherever it lives. On several servers it is a
    /// directory they share, so the index one server builds is the one every
    /// server serves.
    pub pagefind_output_dir: PathBuf,

    /// Secret path segment guarding `POST /cron/{key}`.
    pub cron_key: String,

    /// Security response headers, with the Content-Security-Policy already
    /// assembled. Built once so serving a response is a header clone rather
    /// than two environment reads and a string concatenation.
    pub security_headers: crate::middleware::SecurityHeaders,

    /// Tenant resolution strategy for each request.
    pub tenant_resolution: crate::middleware::TenantResolution,

    /// Request duration above which a request is logged as slow, and above five
    /// times which it is logged as an error.
    pub slow_request_threshold_ms: u128,

    /// Who sees the per-request counts in `Server-Timing` (`SERVER_TIMING`:
    /// `admin`, the default, `all` or `off`). Everyone else gets `total` only,
    /// which is all any visitor got before.
    pub server_timing: crate::middleware::ServerTimingDetail,

    /// Record each statement's fingerprint in the request profile. No
    /// environment variable sets it: the performance tests do, through
    /// `Config`, and span export reads statements for itself.
    pub profile_fingerprints: bool,

    /// Bearer token that reads `/metrics` from any address (`METRICS_TOKEN`,
    /// at least 32 characters). Unset: only a loopback caller with no
    /// forwarding header does; everyone else gets a 404.
    pub metrics_token: Option<String>,

    /// Retention window for the kernel security audit stream, in days.
    pub security_audit_retention_days: i64,

    /// What of a client address may be stored (`CLIENT_IP_STORAGE`: `full`, the
    /// default, `truncated` or `none`). Applied wherever an address is written to
    /// the database, the session index or a log. See [`crate::client_addr`].
    pub client_ip_storage: crate::client_addr::ClientIpStorage,

    /// Whether the session cookie, the only cookie the kernel sets, carries
    /// `Secure` (`COOKIE_SECURE`: `always`, the default, or `never`). `never`
    /// is refused with an `https://` `SITE_URL` or `COOKIE_SAME_SITE=none`.
    /// See [`crate::session::CookieSecure`].
    pub cookie_secure: crate::session::CookieSecure,

    /// The site log keeps at most this many days (`SYSTEM_LOG_RETENTION_DAYS`,
    /// default 30) ...
    pub system_log_retention_days: i64,
    /// ... and at most this many rows (`SYSTEM_LOG_MAX_ROWS`, default
    /// 100,000), whichever is smaller.
    pub system_log_max_rows: i64,

    /// Whether the kernel may ask GitHub whether a newer release exists.
    ///
    /// `UPDATE_CHECK=0` (or `false`/`off`/`no`) turns it off for the process, which
    /// is the switch an air-gapped deployment needs: it must not depend on a
    /// database row being right. There is also an `update_check` site setting, and
    /// the check runs only when both allow it.
    pub update_check_enabled: bool,

    /// The latest-release endpoint the check asks.
    ///
    /// Configurable so a test can point it at a local server, which is also how the
    /// integration test drives the real cron path without touching the network.
    pub update_check_endpoint: String,

    /// Minimum seconds between update checks. Default one day.
    pub update_check_interval_secs: u64,

    /// The site's public base URL, the same value email links are built from.
    ///
    /// Request handling needs it for the absolute URLs that only make sense
    /// off-site: `<link rel="canonical">` and the Open Graph tags, which a
    /// crawler or a link unfurler resolves with no request context to resolve a
    /// relative path against.
    pub site_url: String,

    /// D-26 over-fetch and backfill bounds for access-filtered gather pages.
    pub gather_access: crate::gather::GatherAccessConfig,

    /// How long an AI search query expansion is cached, in seconds.
    ///
    /// `CACHE_TTL_SEARCH_EXPAND`, default 30 days. Long on purpose: an expansion
    /// is a set of related terms for a query, it costs provider tokens to
    /// produce, and it does not go stale the way content does. Zero disables the
    /// cache, which is the switch to reach for when tuning the prompt.
    ///
    /// Lives here rather than in [`CacheConfig`] because request handling reads it
    /// per call; the TTLs there are held by long-lived services instead.
    pub search_expand_cache_ttl: u64,

    /// Process-level override of the built-in cron scheduler's interval
    /// (`CRON_INTERVAL_SECS`), seconds. Unset: the `cron_interval_secs`
    /// `site_config` row decides, edited on the cron report. Set (including to
    /// `0`, which turns the scheduler off): this wins outright, the same
    /// precedence `UPDATE_CHECK` gives the environment over a site setting.
    pub cron_interval_override: Option<u64>,
}

/// The SMTP configuration mail is sent with, as an administrator may see it.
///
/// The environment is the one source of SMTP configuration. This is what the
/// site settings page shows of it: the username and password say only whether
/// they are set, never what they are.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SmtpSummary {
    /// `SMTP_HOST`; `None` means mail is not sent at all.
    pub host: Option<String>,
    /// `SMTP_PORT`.
    pub port: u16,
    /// `SMTP_ENCRYPTION`.
    pub encryption: String,
    /// `SMTP_FROM_EMAIL`.
    pub from: String,
    /// Whether `SMTP_USERNAME` is set.
    pub username_set: bool,
    /// Whether `SMTP_PASSWORD` is set.
    pub password_set: bool,
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

    /// Template search path (default: a single `./templates` entry).
    ///
    /// Read once here rather than by the theme engine, so a caller that wants a
    /// different template root sets a field instead of an environment variable.
    /// Later directories override earlier ones.
    pub templates_dirs: Vec<PathBuf>,

    /// Where the deployment's themes are found (`THEMES_DIR`, default a single
    /// `./themes` entry): each a directory holding `theme.toml`. A search path
    /// like `TEMPLATES_DIR`; a later entry's theme replaces an earlier one of
    /// the same name. See `theme::registry`.
    pub themes_dirs: Vec<PathBuf>,

    /// Proxies whose `X-Forwarded-For` and `X-Forwarded-Proto` headers are
    /// believed (`TRUSTED_PROXIES`: comma-separated addresses and CIDR ranges).
    /// `X-Forwarded-Proto` from one of them is the only way a response gets
    /// `Strict-Transport-Security`.
    ///
    /// Empty means trust none, which is the safe default: an unset
    /// `TRUSTED_PROXIES` must not let any client spoof its own address. An entry
    /// that does not parse fails startup. See [`crate::client_addr`].
    pub trusted_proxies: crate::client_addr::TrustedProxies,

    /// HMAC secret for OAuth2 access tokens. `None` disables OAuth2.
    ///
    /// Kept out of [`RuntimeConfig`] deliberately — it is consumed once at
    /// startup by `OAuthService`, so it never needs to reach a request handler.
    pub jwt_secret: Option<String>,

    /// The 32-byte root key [`crate::secret_box::SecretBox`] derives every
    /// purpose-specific key from (`ENCRYPTION_KEY`, 64 hex characters or
    /// base64). `None` when the site has not set it, which disables TOTP
    /// enrolment rather than failing startup — passkeys still satisfy a
    /// role's second-factor requirement with no key configured. A key that is
    /// set but malformed fails startup with a clear message (B304): a site
    /// that thinks it has encryption at rest and does not is worse than one
    /// that never claimed to.
    pub encryption_key: Option<[u8; 32]>,

    /// How long graceful shutdown waits for in-flight work (default: 30s).
    pub shutdown_timeout: Duration,

    /// Whether `serve` and the other subcommands apply pending migrations when
    /// they start (`MIGRATE_ON_START`, default true). Off, they never change the
    /// schema: `trovato migrate` does, as its own step, and `serve` waits, not
    /// ready, until the schema it needs is there.
    pub migrate_on_start: bool,

    /// How long the server keeps serving, reporting not ready, after SIGTERM
    /// and before it stops accepting (`SHUTDOWN_DRAIN_SECS`). Defaults to 5
    /// seconds under Kubernetes (`KUBERNETES_SERVICE_HOST` set) and 0 elsewhere,
    /// so a single server stops exactly as fast as it always has.
    pub shutdown_drain: Duration,

    /// Bearer token that sees the health details (pool statistics, services)
    /// from any address (`HEALTH_TOKEN`). Unset: only a loopback caller does.
    pub health_token: Option<String>,

    /// Mail outbox, worker and limit settings (BG18). See [`MailConfig`].
    pub mail: MailConfig,

    /// The configuration request handling reads. See [`RuntimeConfig`].
    pub runtime: RuntimeConfig,
}

/// Mail outbox, worker and limit settings (BG18, `docs/design/mail-queue.md`).
///
/// Every limit is a count; 0 means no limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailConfig {
    /// Worker tasks per process (`MAIL_WORKERS`, default 2, at least 1). The
    /// first only sends transactional mail.
    pub workers: usize,
    /// Attempts before a message is marked failed (`MAIL_MAX_ATTEMPTS`, 12).
    pub max_attempts: i32,
    /// Seconds a claim holds a message before another worker may take it
    /// (`MAIL_CLAIM_LEASE_SECS`, 300, at least 60: a worker stops starting sends
    /// at half the lease).
    pub claim_lease_secs: i64,
    /// Seconds between polls when nothing wakes the worker (`MAIL_POLL_SECS`, 2).
    pub poll_secs: u64,
    /// Messages one claim takes on the general lane (`MAIL_BATCH`, 10).
    pub batch: i64,
    /// Days a finished message keeps its body (`MAIL_BODY_RETENTION_DAYS`, 7).
    pub body_retention_days: i64,
    /// Days a finished message is kept at all (`MAIL_LOG_RETENTION_DAYS`, 90).
    pub log_retention_days: i64,
    /// Bulk messages per blog per hour (`MAIL_BLOG_HOURLY_LIMIT`, 1000).
    pub blog_hourly_limit: i64,
    /// Bulk messages per blog per 24 hours (`MAIL_BLOG_DAILY_LIMIT`, 5000).
    pub blog_daily_limit: i64,
    /// Transactional messages per blog per hour
    /// (`MAIL_BLOG_TRANSACTIONAL_HOURLY_LIMIT`, 500).
    pub blog_transactional_hourly_limit: i64,
    /// Bulk messages for the whole network per hour
    /// (`MAIL_NETWORK_HOURLY_LIMIT`, 20000).
    pub network_hourly_limit: i64,
}

impl Default for MailConfig {
    fn default() -> Self {
        Self::from_lookup(&|_| None)
    }
}

impl MailConfig {
    /// Resolve from `lookup`, with the documented defaults.
    pub(crate) fn from_lookup(lookup: Lookup<'_>) -> Self {
        let count = |name: &str, default: i64| parse_or(lookup, name, default).max(0);
        Self {
            workers: parse_positive_or(lookup, "MAIL_WORKERS", 2) as usize,
            max_attempts: parse_positive_or(lookup, "MAIL_MAX_ATTEMPTS", 12) as i32,
            claim_lease_secs: i64::from(
                parse_positive_or(lookup, "MAIL_CLAIM_LEASE_SECS", 300).max(60),
            ),
            poll_secs: u64::from(parse_positive_or(lookup, "MAIL_POLL_SECS", 2)),
            batch: i64::from(parse_positive_or(lookup, "MAIL_BATCH", 10)),
            body_retention_days: count("MAIL_BODY_RETENTION_DAYS", 7),
            log_retention_days: count("MAIL_LOG_RETENTION_DAYS", 90).max(1),
            blog_hourly_limit: count("MAIL_BLOG_HOURLY_LIMIT", 1000),
            blog_daily_limit: count("MAIL_BLOG_DAILY_LIMIT", 5000),
            blog_transactional_hourly_limit: count("MAIL_BLOG_TRANSACTIONAL_HOURLY_LIMIT", 500),
            network_hourly_limit: count("MAIL_NETWORK_HOURLY_LIMIT", 20000),
        }
    }
}

/// The settings a [`Config`] is resolved from.
///
/// Two lookups rather than one because search-path settings are paths: reading
/// them as `OsString` keeps a directory name that is not valid UTF-8 working,
/// which `env::var` would have rejected outright.
pub(crate) struct Settings<'a> {
    /// Look a setting up as a string.
    pub get: Lookup<'a>,
    /// Look a setting up as an OS string. Used for path-valued settings.
    pub get_os: &'a dyn Fn(&str) -> Option<std::ffi::OsString>,
}

impl Settings<'_> {
    /// Resolve a path-valued setting as a platform search path.
    ///
    /// Splits on the platform path separator (`:` on unix, `;` on windows) via
    /// `env::split_paths`, so a plain single-directory value parses to a
    /// one-element list and every pre-existing deployment keeps its old
    /// behaviour. Empty segments are dropped, which is what makes a trailing or
    /// doubled separator harmless rather than a silent "current directory"
    /// entry. Falls back to `default` when unset, or set to something with no
    /// usable segment at all.
    fn search_path(&self, name: &str, default: &str) -> Vec<PathBuf> {
        split_search_path_value((self.get_os)(name).as_deref(), default)
    }

    /// Resolve a comma-separated list, dropping empty entries.
    fn csv(&self, name: &str, default: &[&str]) -> Vec<String> {
        (self.get)(name)
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_else(|| default.iter().map(|s| (*s).to_string()).collect())
    }
}

/// Resolve `COOKIE_SECURE`, refusing a combination that cannot be right.
///
/// `never` with an `https://` `SITE_URL` is refused: that site is served over
/// HTTPS, so dropping `Secure` only lets the session cookie leak over any plain
/// HTTP request to the same host. `never` with `COOKIE_SAME_SITE=none` is
/// refused because browsers reject a `SameSite=None` cookie without `Secure`,
/// so nobody could log in.
pub(crate) fn cookie_secure_from(
    raw: Option<&str>,
    site_url: &str,
    cookie_same_site: &str,
) -> Result<crate::session::CookieSecure> {
    let cookie_secure = crate::session::CookieSecure::parse(raw.unwrap_or_default())
        .map_err(|e| anyhow::anyhow!("COOKIE_SECURE: {e}"))?;
    if cookie_secure == crate::session::CookieSecure::Never {
        if site_url
            .trim()
            .get(..8)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
        {
            anyhow::bail!(
                "COOKIE_SECURE=never is refused because SITE_URL ({site_url}) is https://: \
                 a site served over HTTPS must keep its session cookie Secure. \
                 Unset COOKIE_SECURE, or set it to always."
            );
        }
        if cookie_same_site == "none" {
            anyhow::bail!(
                "COOKIE_SECURE=never is refused with COOKIE_SAME_SITE=none: browsers reject \
                 a SameSite=None cookie that is not Secure, so nobody could log in."
            );
        }
    }
    Ok(cookie_secure)
}

/// The environment lookup for path-valued settings. See [`env_lookup`].
fn env_os_lookup(name: &str) -> Option<std::ffi::OsString> {
    env::var_os(name)
}

/// Parse an already-read search-path value, as documented on
/// [`Settings::search_path`]. `None` is "the setting is not configured".
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
    ///
    /// **This is the one place in the process that reads the environment.**
    /// Everything downstream — request handlers, middleware, the gather
    /// over-fetch loop, the cron tasks — takes its values from the `Config` this
    /// produces or from the [`RuntimeConfig`] carried on `AppState`. That is what
    /// makes those components steerable by a caller, and what removed the last
    /// reason for a test to mutate a process-global.
    ///
    /// A one-line edge over `from_settings`, which holds the resolution.
    pub fn from_env() -> Result<Self> {
        Self::from_settings(&Settings {
            get: &env_lookup,
            get_os: &env_os_lookup,
        })
    }

    /// Resolve the configuration from an arbitrary settings source.
    ///
    /// Holds every setting name and every documented default, so a test can
    /// drive the whole resolution from an explicit map. `DATABASE_URL` is the
    /// only required setting; `PORT`, `DATABASE_MAX_CONNECTIONS` and `SMTP_PORT`
    /// are hard errors when present but unparseable, because silently serving on
    /// the wrong port is worse than refusing to start.
    pub(crate) fn from_settings(settings: &Settings<'_>) -> Result<Self> {
        let get = settings.get;

        let port = match get("PORT") {
            Some(raw) => raw.trim().parse().context("PORT must be a valid u16")?,
            None => 3000,
        };

        let database_url =
            get("DATABASE_URL").context("DATABASE_URL environment variable is required")?;

        let redis_url = get("REDIS_URL").unwrap_or_else(|| "redis://127.0.0.1:6379".to_string());

        let database_max_connections = match get("DATABASE_MAX_CONNECTIONS") {
            Some(raw) => raw
                .trim()
                .parse()
                .context("DATABASE_MAX_CONNECTIONS must be a valid u32")?,
            None => 10,
        };

        // PLUGINS_DIR, TEMPLATES_DIR and STATIC_DIR are search paths, not single
        // directories. Each is split on the platform path separator (`:` on
        // unix), so the historical single-directory value keeps parsing to a
        // one-element list and existing deployments are unaffected.
        let plugins_dirs = settings.search_path("PLUGINS_DIR", "./plugins");
        let templates_dirs = settings.search_path("TEMPLATES_DIR", "./templates");
        let themes_dirs = settings.search_path("THEMES_DIR", "./themes");
        let static_dirs = settings.search_path("STATIC_DIR", "./static");
        let pagefind_output_dir = get("PAGEFIND_OUTPUT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                static_dirs
                    .first()
                    .cloned()
                    .unwrap_or_else(|| PathBuf::from("./static"))
                    .join("pagefind")
            });

        let uploads_dir = get("UPLOADS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("./uploads"));

        let files_url = get("FILES_URL").unwrap_or_else(|| "/files".to_string());

        // Note the empty default: no configured origins means no cross-origin
        // allowance, which `build_cors_layer` turns into a same-origin policy.
        let cors_allowed_origins = settings.csv("CORS_ALLOWED_ORIGINS", &[]);

        let cookie_same_site = get("COOKIE_SAME_SITE")
            .unwrap_or_else(|| "strict".to_string())
            .to_lowercase();

        let disabled_plugins = settings.csv("DISABLED_PLUGINS", &[]);

        let smtp_host = get("SMTP_HOST");

        let smtp_port = match get("SMTP_PORT") {
            Some(raw) => raw
                .trim()
                .parse()
                .context("SMTP_PORT must be a valid u16")?,
            None => 587,
        };

        let smtp_username = get("SMTP_USERNAME");
        let smtp_password = get("SMTP_PASSWORD");

        let smtp_encryption = get("SMTP_ENCRYPTION")
            .unwrap_or_else(|| "starttls".to_string())
            .to_lowercase();

        let smtp_from_email =
            get("SMTP_FROM_EMAIL").unwrap_or_else(|| "noreply@localhost".to_string());

        let site_url = get("SITE_URL").unwrap_or_else(|| format!("http://localhost:{port}"));

        let gather_max_page_size = parse_or(get, "GATHER_MAX_PAGE_SIZE", 100);

        let language_negotiation_methods = settings.csv(
            "LANGUAGE_NEGOTIATION_METHODS",
            &["url_prefix", "accept_header"],
        );

        // Trusted proxies whose X-Forwarded-For is believed. Unset ⇒ trust none,
        // so no client can spoof its own address by default. A bad entry is a
        // hard error: dropping it would quietly stop trusting a real proxy.
        let trusted_proxies =
            crate::client_addr::TrustedProxies::parse(&get("TRUSTED_PROXIES").unwrap_or_default())
                .map_err(|e| anyhow::anyhow!("TRUSTED_PROXIES: {e}"))?;

        // What of a client address may be stored. Unset ⇒ `full`, as before.
        let client_ip_storage = crate::client_addr::ClientIpStorage::parse(
            &get("CLIENT_IP_STORAGE").unwrap_or_default(),
        )
        .map_err(|e| anyhow::anyhow!("CLIENT_IP_STORAGE: {e}"))?;

        // Whether the session cookie is Secure. Unset ⇒ `always`, as before.
        let cookie_secure = cookie_secure_from(
            get("COOKIE_SECURE").as_deref(),
            &site_url,
            &cookie_same_site,
        )?;

        let jwt_secret = get("JWT_SECRET");

        let encryption_key = match get("ENCRYPTION_KEY") {
            None => None,
            Some(raw) => Some(
                crate::secret_box::parse_encryption_key(&raw)
                    .map_err(|e| anyhow::anyhow!("ENCRYPTION_KEY: {e}"))?,
            ),
        };

        let shutdown_timeout = Duration::from_secs(parse_or(get, "SHUTDOWN_TIMEOUT_SECS", 30));
        // Strict: a typo must not quietly turn a single server into one that
        // waits, not ready, for a migration nobody runs.
        let migrate_on_start = match get("MIGRATE_ON_START")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("" | "1" | "true" | "yes" | "on") => true,
            Some("0" | "false" | "no" | "off") => false,
            Some(other) => anyhow::bail!("MIGRATE_ON_START must be true or false, not {other:?}"),
        };
        let default_drain = if get("KUBERNETES_SERVICE_HOST").is_some() {
            5
        } else {
            0
        };
        let shutdown_drain =
            Duration::from_secs(parse_or(get, "SHUTDOWN_DRAIN_SECS", default_drain));
        let health_token = get("HEALTH_TOKEN").filter(|t| !t.trim().is_empty());
        // The probes have no rate limit, so the token must not be guessable.
        if let Some(token) = &health_token
            && token.len() < MIN_HEALTH_TOKEN_LEN
        {
            anyhow::bail!(
                "HEALTH_TOKEN must be at least {MIN_HEALTH_TOKEN_LEN} characters \
                 (for example `openssl rand -hex 32`)"
            );
        }

        let metrics_token = get("METRICS_TOKEN").filter(|t| !t.trim().is_empty());
        // A token that opens `/metrics` from anywhere must not be guessable:
        // only the page rate limit stands in the way.
        if let Some(token) = &metrics_token
            && token.len() < MIN_HEALTH_TOKEN_LEN
        {
            anyhow::bail!(
                "METRICS_TOKEN must be at least {MIN_HEALTH_TOKEN_LEN} characters \
                 (for example `openssl rand -hex 32`)"
            );
        }

        let runtime = RuntimeConfig {
            static_dirs,
            pagefind_output_dir,
            cron_key: get("CRON_KEY").unwrap_or_else(|| "default-cron-key".to_string()),
            security_headers: crate::middleware::SecurityHeaders::from_lookup(get),
            tenant_resolution: crate::middleware::TenantResolution::from_lookup(get),
            slow_request_threshold_ms: parse_or(get, "QUERY_SLOW_THRESHOLD_MS", 100),
            server_timing: crate::middleware::ServerTimingDetail::from_setting(
                get("SERVER_TIMING").as_deref(),
            ),
            profile_fingerprints: false,
            metrics_token,
            security_audit_retention_days: crate::audit::retention_days_from(
                get("SECURITY_AUDIT_RETENTION_DAYS").as_deref(),
            ),
            client_ip_storage,
            cookie_secure,
            system_log_retention_days: parse_or(
                get,
                "SYSTEM_LOG_RETENTION_DAYS",
                crate::system_log::DEFAULT_RETENTION_DAYS,
            ),
            system_log_max_rows: parse_or(
                get,
                "SYSTEM_LOG_MAX_ROWS",
                crate::system_log::DEFAULT_MAX_ROWS,
            ),
            site_url: site_url.clone(),
            gather_access: crate::gather::GatherAccessConfig::from_lookup(get),
            // 30 days.
            search_expand_cache_ttl: parse_or(get, "CACHE_TTL_SEARCH_EXPAND", 2_592_000),
            update_check_enabled: !crate::update_status::env_disables_check(
                get("UPDATE_CHECK").as_deref(),
            ),
            update_check_endpoint: get("UPDATE_CHECK_ENDPOINT")
                .unwrap_or_else(|| crate::update_status::DEFAULT_RELEASE_ENDPOINT.to_string()),
            update_check_interval_secs: parse_or(
                get,
                "UPDATE_CHECK_INTERVAL_SECS",
                crate::update_status::DEFAULT_CHECK_INTERVAL_SECS,
            ),
            cron_interval_override: get("CRON_INTERVAL_SECS")
                .filter(|raw| !raw.trim().is_empty())
                .map(|raw| raw.trim().parse::<u64>())
                .transpose()
                .context("CRON_INTERVAL_SECS must be a valid non-negative integer")?,
        };

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
            templates_dirs,
            themes_dirs,
            trusted_proxies,
            jwt_secret,
            encryption_key,
            shutdown_timeout,
            migrate_on_start,
            shutdown_drain,
            health_token,
            mail: MailConfig::from_lookup(get),
            runtime,
        })
    }
}

impl Config {
    /// The SMTP configuration without its secrets, for display.
    pub fn smtp_summary(&self) -> SmtpSummary {
        SmtpSummary {
            host: self.smtp_host.clone(),
            port: self.smtp_port,
            encryption: self.smtp_encryption.clone(),
            from: self.smtp_from_email.clone(),
            username_set: self.smtp_username.as_deref().is_some_and(|u| !u.is_empty()),
            password_set: self.smtp_password.as_deref().is_some_and(|p| !p.is_empty()),
        }
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod config_tests {
    use super::*;
    use std::collections::HashMap;

    /// Resolve a whole [`Config`] from an explicit settings map.
    ///
    /// This is what makes every setting name and every documented default
    /// assertable without touching the process environment — and, transitively,
    /// what let the last environment-mutating test in the workspace go away.
    fn config_from(pairs: &[(&str, &str)]) -> Result<Config> {
        let settings: HashMap<&str, &str> = pairs.iter().copied().collect();
        let get = |name: &str| settings.get(name).map(|v| (*v).to_string());
        let get_os = |name: &str| settings.get(name).map(std::ffi::OsString::from);
        Config::from_settings(&Settings {
            get: &get,
            get_os: &get_os,
        })
    }

    /// The one required setting, so the happy path below can stay terse.
    fn with_db(extra: &[(&str, &str)]) -> Config {
        let mut pairs = vec![("DATABASE_URL", "postgres://localhost/x")];
        pairs.extend_from_slice(extra);
        config_from(&pairs).expect("config resolves")
    }

    /// `CRON_INTERVAL_SECS` (B178): unset or empty leaves the site setting in
    /// charge; a number, `0` included, overrides it; anything else fails
    /// startup rather than being read as off.
    #[test]
    fn cron_interval_override_setting() {
        assert_eq!(with_db(&[]).runtime.cron_interval_override, None);
        assert_eq!(
            with_db(&[("CRON_INTERVAL_SECS", "")])
                .runtime
                .cron_interval_override,
            None
        );
        assert_eq!(
            with_db(&[("CRON_INTERVAL_SECS", "0")])
                .runtime
                .cron_interval_override,
            Some(0)
        );
        assert_eq!(
            with_db(&[("CRON_INTERVAL_SECS", " 120 ")])
                .runtime
                .cron_interval_override,
            Some(120)
        );
        let err = config_from(&[
            ("DATABASE_URL", "postgres://localhost/x"),
            ("CRON_INTERVAL_SECS", "5m"),
        ])
        .expect_err("a non-number fails")
        .to_string();
        assert!(err.contains("CRON_INTERVAL_SECS"), "{err}");
    }

    /// The drain defaults to a Kubernetes endpoint update only under Kubernetes;
    /// an explicit value wins either way. `MIGRATE_ON_START` reads strictly:
    /// a value that is not a boolean fails startup.
    #[test]
    fn drain_and_migrate_on_start_settings() {
        let k8s = ("KUBERNETES_SERVICE_HOST", "10.0.0.1");
        assert_eq!(with_db(&[k8s]).shutdown_drain, Duration::from_secs(5));
        assert_eq!(
            with_db(&[k8s, ("SHUTDOWN_DRAIN_SECS", "0")]).shutdown_drain,
            Duration::ZERO
        );
        assert_eq!(
            with_db(&[("SHUTDOWN_DRAIN_SECS", "7")]).shutdown_drain,
            Duration::from_secs(7)
        );
        assert!(!with_db(&[("MIGRATE_ON_START", "false")]).migrate_on_start);
        assert!(with_db(&[("MIGRATE_ON_START", "true")]).migrate_on_start);
        assert!(with_db(&[("MIGRATE_ON_START", "")]).migrate_on_start);
        let err = config_from(&[
            ("DATABASE_URL", "postgres://localhost/x"),
            ("MIGRATE_ON_START", "ture"),
        ])
        .expect_err("a typo fails")
        .to_string();
        assert!(
            err.contains("MIGRATE_ON_START") && err.contains("ture"),
            "{err}"
        );
        let token = "0123456789abcdef0123456789abcdef";
        assert!(
            config_from(&[
                ("DATABASE_URL", "postgres://localhost/x"),
                ("HEALTH_TOKEN", &token[1..])
            ])
            .is_err(),
            "31 characters is too short"
        );
        assert_eq!(
            with_db(&[("HEALTH_TOKEN", token)]).health_token.as_deref(),
            Some(token)
        );
        assert!(with_db(&[("HEALTH_TOKEN", " ")]).health_token.is_none());
        let err = config_from(&[
            ("DATABASE_URL", "postgres://localhost/x"),
            ("METRICS_TOKEN", &token[1..]),
        ])
        .expect_err("a 31 character metrics token is too short");
        assert!(err.to_string().contains("METRICS_TOKEN"), "{err}");
        assert_eq!(
            with_db(&[("METRICS_TOKEN", token)])
                .runtime
                .metrics_token
                .as_deref(),
            Some(token)
        );
        for blank in ["", " "] {
            assert!(
                with_db(&[("METRICS_TOKEN", blank)])
                    .runtime
                    .metrics_token
                    .is_none(),
                "{blank:?}"
            );
        }
        let err = config_from(&[
            ("DATABASE_URL", "postgres://localhost/x"),
            ("HEALTH_TOKEN", "t0ken"),
        ])
        .expect_err("a short token fails")
        .to_string();
        assert!(err.contains("HEALTH_TOKEN"), "{err}");
    }

    /// A bad `TRUSTED_PROXIES` entry or `CLIENT_IP_STORAGE` value refuses to
    /// start, naming the setting and the value, rather than being dropped.
    #[test]
    fn a_bad_proxy_range_or_storage_value_fails_startup() {
        let base = ("DATABASE_URL", "postgres://localhost/x");
        let err = config_from(&[base, ("TRUSTED_PROXIES", "10.0.0.1, 10.0.0.0/33")])
            .expect_err("a bad CIDR fails")
            .to_string();
        assert!(
            err.contains("TRUSTED_PROXIES") && err.contains("10.0.0.0/33"),
            "{err}"
        );
        let err = config_from(&[base, ("CLIENT_IP_STORAGE", "hashed")])
            .expect_err("an unknown storage value fails")
            .to_string();
        assert!(
            err.contains("CLIENT_IP_STORAGE") && err.contains("hashed"),
            "{err}"
        );
    }

    /// `PAGEFIND_OUTPUT_DIR` moves the index, say onto a volume every server
    /// shares, without moving the static directories.
    #[test]
    fn pagefind_output_dir_can_be_set() {
        let config = with_db(&[("PAGEFIND_OUTPUT_DIR", "/shared/pagefind")]);
        assert_eq!(
            config.runtime.pagefind_output_dir,
            PathBuf::from("/shared/pagefind")
        );
        assert_eq!(config.runtime.static_dirs, vec![PathBuf::from("./static")]);
    }

    #[test]
    fn cookie_secure_never_is_read_and_always_is_the_default() {
        use crate::session::CookieSecure;
        assert_eq!(
            with_db(&[("COOKIE_SECURE", "always")])
                .runtime
                .cookie_secure,
            CookieSecure::Always
        );
        assert_eq!(
            with_db(&[("COOKIE_SECURE", " NEVER ")])
                .runtime
                .cookie_secure,
            CookieSecure::Never
        );
        // Plain HTTP LAN site: allowed.
        assert_eq!(
            with_db(&[
                ("COOKIE_SECURE", "never"),
                ("SITE_URL", "http://192.168.1.20:3000"),
            ])
            .runtime
            .cookie_secure,
            CookieSecure::Never
        );
    }

    #[test]
    fn cookie_secure_rejects_an_unknown_value() {
        for value in ["auto", "false", "yes"] {
            let err = config_from(&[("DATABASE_URL", "postgres://x"), ("COOKIE_SECURE", value)])
                .expect_err("an unknown COOKIE_SECURE must fail startup");
            assert!(err.to_string().contains("COOKIE_SECURE"), "got: {err}");
        }
    }

    #[test]
    fn cookie_secure_never_with_an_https_site_url_fails_startup() {
        for site_url in [
            "https://example.com",
            "HTTPS://example.com/",
            " https://lan.test",
        ] {
            let err = config_from(&[
                ("DATABASE_URL", "postgres://x"),
                ("COOKIE_SECURE", "never"),
                ("SITE_URL", site_url),
            ])
            .expect_err("never with an https SITE_URL must fail startup");
            let message = err.to_string();
            assert!(
                message.contains("COOKIE_SECURE=never") && message.contains("SITE_URL"),
                "got: {message}"
            );
        }
        // `always` with the same URL is fine.
        assert!(
            config_from(&[
                ("DATABASE_URL", "postgres://x"),
                ("COOKIE_SECURE", "always"),
                ("SITE_URL", "https://example.com"),
            ])
            .is_ok()
        );
    }

    #[test]
    fn cookie_secure_never_with_same_site_none_fails_startup() {
        let err = config_from(&[
            ("DATABASE_URL", "postgres://x"),
            ("COOKIE_SECURE", "never"),
            ("COOKIE_SAME_SITE", "None"),
        ])
        .expect_err("never with SameSite=None must fail startup");
        assert!(err.to_string().contains("COOKIE_SAME_SITE"), "got: {err}");
    }
    #[test]
    fn database_url_is_required() {
        let err = config_from(&[]).expect_err("no DATABASE_URL must fail");
        assert!(err.to_string().contains("DATABASE_URL"), "got: {err}");
    }

    /// Every documented default, asserted against a lookup that returns nothing.
    #[test]
    fn nothing_configured_yields_the_documented_defaults() {
        let config = with_db(&[]);
        assert_eq!(config.port, 3000);
        assert_eq!(config.redis_url, "redis://127.0.0.1:6379");
        assert_eq!(config.database_max_connections, 10);
        assert_eq!(config.plugins_dirs, vec![PathBuf::from("./plugins")]);
        assert_eq!(config.templates_dirs, vec![PathBuf::from("./templates")]);
        assert_eq!(config.uploads_dir, PathBuf::from("./uploads"));
        assert_eq!(config.files_url, "/files");
        assert!(config.cors_allowed_origins.is_empty());
        assert_eq!(config.cookie_same_site, "strict");
        assert!(config.disabled_plugins.is_empty());
        assert_eq!(config.smtp_port, 587);
        assert_eq!(config.smtp_encryption, "starttls");
        assert_eq!(config.smtp_from_email, "noreply@localhost");
        assert_eq!(config.site_url, "http://localhost:3000");
        assert_eq!(config.gather_max_page_size, 100);
        assert_eq!(
            config.language_negotiation_methods,
            vec!["url_prefix".to_string(), "accept_header".to_string()]
        );
        assert_eq!(config.shutdown_timeout, Duration::from_secs(30));
        assert!(
            config.migrate_on_start,
            "unset MIGRATE_ON_START keeps migrating at start"
        );
        assert_eq!(
            config.shutdown_drain,
            Duration::ZERO,
            "no drain outside Kubernetes"
        );
        assert!(config.health_token.is_none());
        assert!(config.jwt_secret.is_none());
        // Unset TRUSTED_PROXIES must trust nothing (RATE-1), not everything.
        assert!(config.trusted_proxies.is_empty());
        // Unset CLIENT_IP_STORAGE keeps storing what it always did.
        assert_eq!(
            config.runtime.client_ip_storage,
            crate::client_addr::ClientIpStorage::Full
        );
        // Unset COOKIE_SECURE keeps the session cookie Secure.
        assert_eq!(
            config.runtime.cookie_secure,
            crate::session::CookieSecure::Always
        );

        // The values request handling reads.
        assert_eq!(config.runtime.static_dirs, vec![PathBuf::from("./static")]);
        assert_eq!(
            config.runtime.pagefind_output_dir,
            PathBuf::from("./static/pagefind")
        );
        assert_eq!(config.runtime.cron_key, "default-cron-key");
        assert_eq!(config.runtime.slow_request_threshold_ms, 100);
        assert_eq!(
            config.runtime.security_audit_retention_days,
            crate::audit::DEFAULT_RETENTION_DAYS
        );
        assert_eq!(
            config.runtime.tenant_resolution,
            crate::middleware::TenantResolution::Default
        );
        assert_eq!(
            config.runtime.gather_access,
            crate::gather::GatherAccessConfig::default()
        );
    }

    /// `SITE_URL` defaults to the *configured* port, not the default port.
    #[test]
    fn site_url_default_follows_the_configured_port() {
        assert_eq!(
            with_db(&[("PORT", "8080")]).site_url,
            "http://localhost:8080"
        );
        assert_eq!(
            with_db(&[("PORT", "8080"), ("SITE_URL", "https://example.com")]).site_url,
            "https://example.com"
        );
    }

    /// The three settings that refuse to start rather than guess. Serving on the
    /// wrong port is worse than not serving.
    #[test]
    fn unparseable_scalars_are_startup_errors() {
        for (name, message) in [
            ("PORT", "PORT"),
            ("DATABASE_MAX_CONNECTIONS", "DATABASE_MAX_CONNECTIONS"),
            ("SMTP_PORT", "SMTP_PORT"),
        ] {
            let err = config_from(&[("DATABASE_URL", "postgres://localhost/x"), (name, "http")])
                .expect_err("{name} must be a startup error");
            assert!(err.to_string().contains(message), "got: {err}");
        }
    }

    /// Every search-path setting is a search path, not a single directory.
    #[test]
    fn search_path_settings_split() {
        let config = with_db(&[
            ("PLUGINS_DIR", "/srv/plugins:/opt/app/plugins"),
            ("TEMPLATES_DIR", "/srv/templates:/opt/app/templates"),
            ("STATIC_DIR", "/srv/static:/opt/app/static"),
        ]);
        assert_eq!(
            config.plugins_dirs,
            vec![
                PathBuf::from("/srv/plugins"),
                PathBuf::from("/opt/app/plugins")
            ]
        );
        assert_eq!(
            config.templates_dirs,
            vec![
                PathBuf::from("/srv/templates"),
                PathBuf::from("/opt/app/templates")
            ]
        );
        assert_eq!(
            config.runtime.static_dirs,
            vec![
                PathBuf::from("/srv/static"),
                PathBuf::from("/opt/app/static")
            ]
        );
        // The index goes under the base (first) static directory.
        assert_eq!(
            config.runtime.pagefind_output_dir,
            PathBuf::from("/srv/static/pagefind")
        );
    }

    /// Comma-separated lists drop empty entries rather than yielding blanks — a
    /// trailing comma is a typo, not a plugin named "".
    #[test]
    fn comma_separated_lists_drop_blanks() {
        let config = with_db(&[
            ("DISABLED_PLUGINS", "trovato_blog, ,trovato_search,"),
            ("LANGUAGE_NEGOTIATION_METHODS", "cookie , url_prefix"),
        ]);
        assert_eq!(
            config.disabled_plugins,
            vec!["trovato_blog", "trovato_search"]
        );
        assert_eq!(
            config.language_negotiation_methods,
            vec!["cookie", "url_prefix"]
        );
    }

    /// Each setting the runtime used to re-read is now resolved here, under its
    /// documented name.
    #[test]
    fn runtime_settings_are_resolved_under_their_documented_names() {
        let config = with_db(&[
            ("CRON_KEY", "s3cret"),
            ("QUERY_SLOW_THRESHOLD_MS", "250"),
            ("SECURITY_AUDIT_RETENTION_DAYS", "30"),
            ("TENANT_RESOLUTION_METHOD", "header"),
            ("GATHER_ACCESS_MAX_SCAN", "77"),
            ("TRUSTED_PROXIES", "127.0.0.1, 10.0.0.0/8"),
            ("CLIENT_IP_STORAGE", "truncated"),
            ("JWT_SECRET", "0123456789abcdef0123456789abcdef"),
            ("SHUTDOWN_TIMEOUT_SECS", "5"),
        ]);
        assert_eq!(config.runtime.cron_key, "s3cret");
        assert_eq!(config.runtime.slow_request_threshold_ms, 250);
        assert_eq!(config.runtime.security_audit_retention_days, 30);
        assert_eq!(
            config.runtime.tenant_resolution,
            crate::middleware::TenantResolution::Header
        );
        assert_eq!(config.runtime.gather_access.max_scan, 77);
        assert_eq!(config.trusted_proxies.len(), 2);
        assert_eq!(
            config.runtime.client_ip_storage,
            crate::client_addr::ClientIpStorage::Truncated
        );
        assert_eq!(
            config.jwt_secret.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(config.shutdown_timeout, Duration::from_secs(5));
    }

    /// `ENCRYPTION_KEY` unset leaves TOTP enrolment unavailable rather than
    /// failing startup; set and well-formed, it resolves to 32 bytes however
    /// it was spelled; set and malformed, it fails startup with a clear
    /// reason rather than silently disabling encryption a site believes it
    /// configured.
    #[test]
    fn encryption_key_unset_is_none_and_malformed_fails_startup() {
        assert!(with_db(&[]).encryption_key.is_none());

        let hex_key = "ab".repeat(32);
        assert_eq!(
            with_db(&[("ENCRYPTION_KEY", &hex_key)])
                .encryption_key
                .map(|k| k.to_vec()),
            Some(vec![0xab; 32])
        );

        use base64::Engine;
        let b64_key = base64::engine::general_purpose::STANDARD.encode([3u8; 32]);
        assert_eq!(
            with_db(&[("ENCRYPTION_KEY", &b64_key)])
                .encryption_key
                .map(|k| k.to_vec()),
            Some(vec![3u8; 32])
        );

        let err = config_from(&[
            ("DATABASE_URL", "postgres://localhost/x"),
            ("ENCRYPTION_KEY", "too-short"),
        ])
        .expect_err("a malformed ENCRYPTION_KEY must fail startup");
        assert!(err.to_string().contains("ENCRYPTION_KEY"));
    }

    /// A zero-day retention window would prune the entire audit stream, so it
    /// falls back rather than being honoured.
    #[test]
    fn a_non_positive_retention_window_falls_back() {
        for bad in ["0", "-30", "forever"] {
            assert_eq!(
                with_db(&[("SECURITY_AUDIT_RETENTION_DAYS", bad)])
                    .runtime
                    .security_audit_retention_days,
                crate::audit::DEFAULT_RETENTION_DAYS,
                "{bad:?} must not shorten retention"
            );
        }
    }

    /// The environment edge itself: that `Config::from_env` reads the real
    /// process environment, and reads it under the names `from_settings` uses.
    ///
    /// The only test in the workspace that mutates the environment. Everything
    /// else drives `from_settings` with an explicit map, which is why this can be
    /// one narrow test rather than a pattern. It goes through `EnvGuard`, so the
    /// write is serialized against every other mutation in the process and is
    /// restored on drop even if an assert below fails.
    #[test]
    fn from_env_reads_the_process_environment() {
        let mut env = trovato_test_utils::env::EnvGuard::new();
        env.set("DATABASE_URL", "postgres://localhost/from_env_probe");
        env.set("CRON_KEY", "from-the-environment");
        env.set("STATIC_DIR", "/srv/from_env_probe");

        let config = Config::from_env().expect("config resolves from the environment");
        assert_eq!(config.database_url, "postgres://localhost/from_env_probe");
        assert_eq!(config.runtime.cron_key, "from-the-environment");
        assert_eq!(
            config.runtime.static_dirs,
            vec![PathBuf::from("/srv/from_env_probe")]
        );
    }
}
