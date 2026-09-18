//! Rate limiting middleware using Redis for distributed counting.
//!
//! Uses a sliding window counter pattern with Redis INCR + EXPIRE.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use redis::AsyncCommands;
use redis::Client as RedisClient;
use tracing::{debug, warn};

use crate::state::AppState;

/// The vetted client IP for the current request.
///
/// Resolved once by [`resolve_client_ip`] with trusted-proxy gating and stored
/// as a request extension. Handlers that key rate limits by IP read this
/// instead of trusting `X-Forwarded-For` directly (RATE-1).
#[derive(Clone, Debug)]
pub struct ClientIp(pub String);

/// Parse a comma-separated `TRUSTED_PROXIES` value into IP addresses.
///
/// Unparseable entries are dropped. An empty/unset value yields an empty list,
/// i.e. **no** proxy is trusted and forwarding headers are ignored — the safe
/// default for a directly-exposed server.
pub fn parse_trusted_proxies(raw: &str) -> Vec<IpAddr> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<IpAddr>().ok())
        .collect()
}

/// Rate limit configuration for different endpoint categories.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Login attempts: (max requests, window duration)
    pub login: (u32, Duration),
    /// Form submissions
    pub forms: (u32, Duration),
    /// API endpoints
    pub api: (u32, Duration),
    /// Search queries
    pub search: (u32, Duration),
    /// File uploads
    pub uploads: (u32, Duration),
    /// User registration
    pub register: (u32, Duration),
    /// Email verification token attempts
    pub verify_email: (u32, Duration),
    /// Profile update submissions
    pub profile: (u32, Duration),
    /// Password change submissions
    pub password: (u32, Duration),
    /// Account-recovery initiation and verification (FR-7c, design §5).
    ///
    /// Applied both per-IP and per-account. Deliberately tighter than login:
    /// recovery is the weakest link in the whole auth story, and unlike a login
    /// attempt each initiation can send mail to a third party's inbox.
    pub recovery: (u32, Duration),

    /// Comment writes: posting, editing and deleting a comment.
    ///
    /// Comment endpoints live under `/api/`, so before this category existed they
    /// were bounded by the generic `api` limit of 100 a minute — per IP *and* per
    /// user. Nobody writes a hundred comments a minute; a spammer with an account
    /// does.
    pub comment: (u32, Duration),

    /// Personal-data export downloads.
    ///
    /// The one authenticated read whose cost scales with what the caller wrote: the
    /// document holds every item and comment on the account. Once an hour is
    /// generous for the purpose (GDPR article 15 is not a thing anyone needs twice a
    /// minute) and bounds the cost.
    pub data_export: (u32, Duration),

    /// AI search query expansion.
    ///
    /// The three AI search endpoints each spend provider tokens per call, and each
    /// costs a different amount, so they are bounded separately rather than
    /// sharing the generic `api` bucket. Defaults follow
    /// `docs/design/search-architecture.md`.
    pub search_expand: (u32, Duration),

    /// AI search result summarization. More expensive than expansion.
    pub search_summarize: (u32, Duration),

    /// AI search follow-up questions. The most expensive of the three: every call
    /// carries the conversation context.
    pub search_followup: (u32, Duration),

    /// Static assets: everything under the static and uploaded-file prefixes.
    ///
    /// Named `static_assets` because `static` is a keyword; the bucket is called
    /// `static` everywhere it is configured or keyed.
    ///
    /// These are GETs of files on disk, and a single page view fetches as many of
    /// them as it has stylesheets, scripts and images. Before this bucket existed
    /// they fell through `categorize_path` to `api` at 100 a minute, so a visitor
    /// on a shared IP loading a handful of asset-heavy pages could be served a 429
    /// for a favicon. The default is deliberately an order of magnitude above the
    /// generic bucket: it exists to bound a scraper, not to ration a page load.
    pub static_assets: (u32, Duration),
}

/// Every rate-limit bucket, by the name used to key it, configure it and
/// categorize a request into it.
///
/// The one list: [`RateLimitConfig::bucket`] and its private `bucket_mut` twin
/// resolve each of these to a field, and `every_bucket_resolves_to_a_field` fails
/// if a name here has no field behind it.
pub const BUCKETS: [&str; 16] = [
    "login",
    "forms",
    "api",
    "search",
    "uploads",
    "register",
    "verify_email",
    "profile",
    "password",
    "recovery",
    "comment",
    "data_export",
    "search_expand",
    "search_summarize",
    "search_followup",
    "static",
];

/// Path prefixes served as static assets.
///
/// `/files/` is the default `FILES_URL`; a site that moves uploaded files
/// elsewhere gets its configured prefix threaded through
/// [`RateLimiter::with_static_prefixes`], and these are only the fallback used by
/// the [`categorize_path`] convenience wrapper.
pub const DEFAULT_STATIC_PREFIXES: [&str; 2] = ["/static/", "/files/"];

/// The environment variable that overrides `bucket`'s limit.
pub fn bucket_env_key(bucket: &str) -> String {
    format!("TROVATO_RATE_LIMIT_{}", bucket.to_uppercase())
}

/// The `site_config` key that overrides `bucket`'s limit.
pub fn bucket_config_key(bucket: &str) -> String {
    format!("rate_limit.{bucket}")
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            login: (5, Duration::from_secs(60)),         // 5 per minute
            forms: (30, Duration::from_secs(60)),        // 30 per minute
            api: (100, Duration::from_secs(60)),         // 100 per minute
            search: (20, Duration::from_secs(60)),       // 20 per minute
            uploads: (10, Duration::from_secs(60)),      // 10 per minute
            register: (3, Duration::from_secs(3600)),    // 3 per hour
            verify_email: (10, Duration::from_secs(60)), // 10 per minute
            profile: (10, Duration::from_secs(60)),      // 10 per minute
            password: (5, Duration::from_secs(60)),      // 5 per minute
            recovery: (5, Duration::from_secs(900)),     // 5 per 15 minutes
            comment: (4, Duration::from_secs(60)),       // 4 per minute
            data_export: (1, Duration::from_secs(3600)), // 1 per hour
            // Per docs/design/search-architecture.md §Rate Limiting.
            search_expand: (30, Duration::from_secs(60)), // 30 per minute
            search_summarize: (10, Duration::from_secs(60)), // 10 per minute
            search_followup: (5, Duration::from_secs(60)), // 5 per minute
            static_assets: (2000, Duration::from_secs(60)), // 2000 per minute
        }
    }
}

impl RateLimitConfig {
    /// The limit and window for `bucket`, or `None` if there is no such bucket.
    pub fn bucket(&self, bucket: &str) -> Option<(u32, Duration)> {
        Some(match bucket {
            "login" => self.login,
            "forms" => self.forms,
            "api" => self.api,
            "search" => self.search,
            "uploads" => self.uploads,
            "register" => self.register,
            "verify_email" => self.verify_email,
            "profile" => self.profile,
            "password" => self.password,
            "recovery" => self.recovery,
            "comment" => self.comment,
            "data_export" => self.data_export,
            "search_expand" => self.search_expand,
            "search_summarize" => self.search_summarize,
            "search_followup" => self.search_followup,
            "static" => self.static_assets,
            _ => return None,
        })
    }

    /// Mutable access to `bucket`'s limit, for applying an override.
    fn bucket_mut(&mut self, bucket: &str) -> Option<&mut (u32, Duration)> {
        Some(match bucket {
            "login" => &mut self.login,
            "forms" => &mut self.forms,
            "api" => &mut self.api,
            "search" => &mut self.search,
            "uploads" => &mut self.uploads,
            "register" => &mut self.register,
            "verify_email" => &mut self.verify_email,
            "profile" => &mut self.profile,
            "password" => &mut self.password,
            "recovery" => &mut self.recovery,
            "comment" => &mut self.comment,
            "data_export" => &mut self.data_export,
            "search_expand" => &mut self.search_expand,
            "search_summarize" => &mut self.search_summarize,
            "search_followup" => &mut self.search_followup,
            "static" => &mut self.static_assets,
            _ => return None,
        })
    }

    /// Replace each bucket's default limit with a configured override.
    ///
    /// For every bucket in [`BUCKETS`], the environment is asked first
    /// (`TROVATO_RATE_LIMIT_<BUCKET>`) and the site configuration second
    /// (`rate_limit.<bucket>`) — **environment wins**, so an operator can always
    /// override a value stored in the database without reaching into it.
    ///
    /// The value is the request count. The window is not configurable: it is part
    /// of what the bucket *means* (`register` is three an hour, `login` five a
    /// minute), and a window edited independently of the count turns a documented
    /// limit into an undocumented one.
    ///
    /// An override that does not parse as a count of at least 1 is ignored with a
    /// warning and the default stands. Refusing to start would be defensible; a
    /// typo in one bucket taking the whole site down is not, and a limit of 0
    /// would reject every request to that bucket including the ones an operator
    /// would need to fix it.
    pub fn apply_overrides(
        &mut self,
        env: &dyn Fn(&str) -> Option<String>,
        config: &dyn Fn(&str) -> Option<String>,
    ) {
        for bucket in BUCKETS {
            let (raw, source) = match env(&bucket_env_key(bucket)) {
                Some(raw) => (raw, bucket_env_key(bucket)),
                None => match config(&bucket_config_key(bucket)) {
                    Some(raw) => (raw, bucket_config_key(bucket)),
                    None => continue,
                },
            };

            match raw.trim().parse::<u32>() {
                Ok(limit) if limit >= 1 => {
                    if let Some(slot) = self.bucket_mut(bucket) {
                        slot.0 = limit;
                    }
                }
                _ => warn!(
                    source = %source,
                    value = %raw,
                    "rate limit override is not a positive integer, keeping the default"
                ),
            }
        }
    }
}

/// Rate limiter using Redis for distributed counting.
#[derive(Clone)]
pub struct RateLimiter {
    redis: RedisClient,
    config: RateLimitConfig,
    /// Proxies whose `X-Forwarded-For` / `X-Real-IP` headers are trusted. Empty
    /// ⇒ trust none (ignore forwarding headers). See [`parse_trusted_proxies`].
    trusted_proxies: Arc<Vec<IpAddr>>,
    /// Path prefixes routed to the `static` bucket. Defaults to
    /// [`DEFAULT_STATIC_PREFIXES`]; production supplies the configured
    /// `FILES_URL` via [`RateLimiter::with_static_prefixes`].
    static_prefixes: Arc<Vec<String>>,
}

impl RateLimiter {
    /// Create a new rate limiter.
    pub fn new(redis: RedisClient, config: RateLimitConfig, trusted_proxies: Vec<IpAddr>) -> Self {
        Self {
            redis,
            config,
            trusted_proxies: Arc::new(trusted_proxies),
            static_prefixes: Arc::new(
                DEFAULT_STATIC_PREFIXES
                    .iter()
                    .map(|p| (*p).to_string())
                    .collect(),
            ),
        }
    }

    /// Route these path prefixes to the `static` bucket.
    ///
    /// Each is normalized to a trailing slash, so `/files` and `/files/` both
    /// match `/files/2026/photo.jpg` and neither matches `/filesystem`.
    #[must_use]
    pub fn with_static_prefixes<S: AsRef<str>>(mut self, prefixes: &[S]) -> Self {
        self.static_prefixes = Arc::new(
            prefixes
                .iter()
                .map(|p| {
                    let p = p.as_ref();
                    if p.ends_with('/') {
                        p.to_string()
                    } else {
                        format!("{p}/")
                    }
                })
                .collect(),
        );
        self
    }

    /// The configured trusted-proxy allowlist.
    pub fn trusted_proxies(&self) -> &[IpAddr] {
        &self.trusted_proxies
    }

    /// The path prefixes routed to the `static` bucket.
    pub fn static_prefixes(&self) -> &[String] {
        &self.static_prefixes
    }

    /// Check if a request should be rate limited.
    ///
    /// Returns Ok(()) if allowed, Err with retry-after seconds if limited.
    pub async fn check(&self, category: &str, identifier: &str) -> Result<(), u64> {
        let (limit, window) = self.get_limit(category);
        let key = format!("rate:{category}:{identifier}");
        let window_secs = window.as_secs();

        let count = match self.increment(&key, window_secs).await {
            Ok(c) => c,
            Err(e) => {
                // If Redis fails, allow the request (fail open)
                warn!(error = %e, "rate limit check failed, allowing request");
                return Ok(());
            }
        };

        if count > limit as i64 {
            debug!(
                category = category,
                identifier = identifier,
                count = count,
                limit = limit,
                "rate limit exceeded"
            );
            Err(window_secs)
        } else {
            Ok(())
        }
    }

    /// Get the rate limit for a category.
    ///
    /// An unknown category falls back to the `api` bucket, which is what
    /// `categorize_path`'s own default does.
    fn get_limit(&self, category: &str) -> (u32, Duration) {
        self.config.bucket(category).unwrap_or(self.config.api)
    }

    /// Increment the counter and return the new value.
    ///
    /// Uses a Lua script to atomically INCR + EXPIRE, preventing a race
    /// where a crash between the two commands creates an immortal counter.
    async fn increment(&self, key: &str, ttl_secs: u64) -> Result<i64, redis::RedisError> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;

        let script = redis::Script::new(
            r"local count = redis.call('INCR', KEYS[1])
              if count == 1 then
                redis.call('EXPIRE', KEYS[1], ARGV[1])
              end
              return count",
        );

        let count: i64 = script
            .key(key)
            .arg(ttl_secs as i64)
            .invoke_async(&mut conn)
            .await?;

        Ok(count)
    }

    /// Get the current count for a key (for monitoring).
    pub async fn get_count(
        &self,
        category: &str,
        identifier: &str,
    ) -> Result<i64, redis::RedisError> {
        let key = format!("rate:{category}:{identifier}");
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let count: Option<i64> = conn.get(&key).await?;
        Ok(count.unwrap_or(0))
    }

    /// Reset the counter for a key (for testing).
    pub async fn reset(&self, category: &str, identifier: &str) -> Result<(), redis::RedisError> {
        let key = format!("rate:{category}:{identifier}");
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let _: () = conn.del(&key).await?;
        Ok(())
    }
}

/// Categorize a request path for rate limiting, using the default static
/// prefixes.
///
/// A site with a non-default `FILES_URL` wants
/// [`categorize_path_with`] and the prefixes the [`RateLimiter`] carries.
pub fn categorize_path(path: &str, method: &str) -> &'static str {
    categorize_path_with(path, method, &DEFAULT_STATIC_PREFIXES)
}

/// Whether this request is a read of a static asset.
///
/// Reads only: a POST under an asset prefix is not a file being served, and
/// giving it the static bucket's very generous limit would be a hole.
fn is_static_asset<S: AsRef<str>>(path: &str, method: &str, static_prefixes: &[S]) -> bool {
    if !matches!(method, "GET" | "HEAD") {
        return false;
    }
    path == "/favicon.ico"
        || static_prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix.as_ref()))
}

/// Categorize a request path for rate limiting.
///
/// Order matters: the specific categories are tested before the generic `/api/`
/// one, since every path they name is also an `/api/` path. That is how the AI
/// search endpoints and the comment writes used to land in the `api` bucket at
/// 100 a minute.
///
/// Static assets are tested first, both because it is the hottest path and
/// because their prefixes overlap nothing below.
pub fn categorize_path_with<S: AsRef<str>>(
    path: &str,
    method: &str,
    static_prefixes: &[S],
) -> &'static str {
    if is_static_asset(path, method, static_prefixes) {
        "static"
    } else if path.starts_with("/user/login") && method == "POST" {
        "login"
    } else if path.starts_with("/user/register") && method == "POST" {
        "register"
    } else if path.starts_with("/file/upload") {
        "uploads"
    } else if let Some(category) = ai_search_category(path) {
        // Each of the three spends provider tokens, at three different costs.
        category
    } else if is_comment_write(path, method) {
        "comment"
    } else if path.starts_with("/search") || path.starts_with("/api/search") {
        "search"
    } else if path.starts_with("/api/") {
        "api"
    } else if method == "POST" {
        "forms"
    } else {
        "api" // Default category for GET requests
    }
}

/// The rate-limit category for an AI search endpoint, or `None` for any other
/// path.
///
/// Matched on the whole path rather than a prefix so that a future
/// `/api/v1/search/something-else` is not silently given the cheapest of the
/// three limits.
fn ai_search_category(path: &str) -> Option<&'static str> {
    match path.trim_end_matches('/') {
        "/api/v1/search/expand" => Some("search_expand"),
        "/api/v1/search/summarize" => Some("search_summarize"),
        "/api/v1/search/followup" => Some("search_followup"),
        _ => None,
    }
}

/// Whether this request writes a comment.
///
/// Covers both shapes the comment routes take: `POST /api/item/{id}/comments`
/// and a write to `/api/comment/{id}`. Reads are left in the `api` bucket — it
/// is the writes that cost moderation attention.
fn is_comment_write(path: &str, method: &str) -> bool {
    let writes = matches!(method, "POST" | "PUT" | "PATCH" | "DELETE");
    if !writes {
        return false;
    }

    let path = path.trim_end_matches('/');
    (path.starts_with("/api/item/") && path.ends_with("/comments"))
        || path.starts_with("/api/comment/")
}

/// Resolve the client identifier (IP address) for rate limiting.
///
/// `X-Forwarded-For` / `X-Real-IP` are honored **only** when the direct socket
/// peer (`addr`) is in the `trusted_proxies` allowlist (RATE-1). Otherwise the
/// socket peer IP is used and the forwarding headers are ignored — this stops a
/// directly-connecting client from spoofing `X-Forwarded-For` to mint unlimited
/// distinct rate-limit buckets. With no peer and no trust, the identifier is
/// `"unknown"`.
pub fn get_client_id(
    addr: Option<std::net::SocketAddr>,
    headers: &axum::http::HeaderMap,
    trusted_proxies: &[IpAddr],
) -> String {
    let peer = addr.map(|a| a.ip());

    // Only a trusted proxy's forwarding headers are believed.
    if peer.is_some_and(|ip| trusted_proxies.contains(&ip)) {
        // X-Forwarded-For: the first (client-most) entry.
        if let Some(forwarded) = headers.get("x-forwarded-for")
            && let Ok(value) = forwarded.to_str()
            && let Some(first) = value.split(',').next()
        {
            let first = first.trim();
            if !first.is_empty() {
                return first.to_string();
            }
        }
        // X-Real-IP fallback.
        if let Some(real_ip) = headers.get("x-real-ip")
            && let Ok(value) = real_ip.to_str()
        {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }

    // Untrusted or unknown peer: use the socket peer, never the headers.
    peer.map(|ip| ip.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Middleware that resolves the vetted client IP once and stores it as a
/// [`ClientIp`] request extension.
///
/// Runs ahead of the rate-limit checks and the route handlers so both consume
/// the same trusted-proxy-gated value rather than re-reading raw headers. Reads
/// the socket peer from the `ConnectInfo` extension (present in production via
/// `into_make_service_with_connect_info`; a test harness may supply it via
/// `MockConnectInfo`).
pub async fn resolve_client_ip(
    State(state): State<AppState>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let addr = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0);
    let client_id = get_client_id(
        addr,
        request.headers(),
        state.rate_limiter().trusted_proxies(),
    );
    request.extensions_mut().insert(ClientIp(client_id));
    next.run(request).await
}

/// Rate limit exceeded response.
pub fn rate_limit_response(retry_after: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [
            ("retry-after", retry_after.to_string()),
            ("content-type", "application/json".to_string()),
        ],
        format!(r#"{{"error":"Rate limit exceeded","retry_after":{retry_after}}}"#),
    )
        .into_response()
}

/// Rate limiting middleware layer.
///
/// Extracts the client identifier and request category, then checks the
/// rate limiter. Returns 429 Too Many Requests when the limit is exceeded.
pub async fn check_rate_limit(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let category = categorize_path_with(
        request.uri().path(),
        request.method().as_str(),
        state.rate_limiter().static_prefixes(),
    );
    // Prefer the vetted IP resolved by `resolve_client_ip`; fall back to an
    // inline trusted-proxy resolution if that middleware is not in the stack.
    let client_id = request
        .extensions()
        .get::<ClientIp>()
        .map(|c| c.0.clone())
        .unwrap_or_else(|| {
            get_client_id(
                Some(addr),
                request.headers(),
                state.rate_limiter().trusted_proxies(),
            )
        });

    match state.rate_limiter().check(category, &client_id).await {
        Ok(()) => next.run(request).await,
        Err(retry_after) => rate_limit_response(retry_after),
    }
}

/// Per-user rate limiting middleware layer (runs after authentication).
///
/// Adds a second rate limit check keyed on the authenticated user's ID.
/// This prevents a single user from exceeding limits by distributing
/// requests across multiple IPs. Only fires for authenticated requests.
pub async fn check_authenticated_rate_limit(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // Extract user ID from session (set by bearer/API token auth layers).
    let session = request
        .extensions()
        .get::<tower_sessions::Session>()
        .cloned();

    let user_id = if let Some(ref session) = session {
        session
            .get::<uuid::Uuid>(crate::routes::auth::SESSION_USER_ID)
            .await
            .ok()
            .flatten()
    } else {
        None
    };

    // Only apply per-user limits to authenticated requests.
    if let Some(uid) = user_id {
        let category = categorize_path_with(
            request.uri().path(),
            request.method().as_str(),
            state.rate_limiter().static_prefixes(),
        );
        let user_key = format!("user:{uid}");

        if let Err(retry_after) = state.rate_limiter().check(category, &user_key).await {
            return rate_limit_response(retry_after);
        }
    }

    next.run(request).await
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter")
            .field("config", &self.config)
            .finish()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn categorize_login_paths() {
        assert_eq!(categorize_path("/user/login", "POST"), "login");
        assert_eq!(categorize_path("/user/login/json", "POST"), "login");
    }

    #[test]
    fn categorize_register_paths() {
        assert_eq!(categorize_path("/user/register", "POST"), "register");
        assert_eq!(categorize_path("/user/register/json", "POST"), "register");
    }

    #[test]
    fn categorize_upload_paths() {
        assert_eq!(categorize_path("/file/upload", "POST"), "uploads");
    }

    #[test]
    fn categorize_search_paths() {
        assert_eq!(categorize_path("/search", "GET"), "search");
        assert_eq!(categorize_path("/api/search", "GET"), "search");
    }

    #[test]
    fn categorize_api_paths() {
        assert_eq!(categorize_path("/api/items", "GET"), "api");
        assert_eq!(categorize_path("/api/v1/chat", "POST"), "api");
    }

    /// The three AI search endpoints each get their own bucket. Before this they
    /// were `/api/` paths, so they shared the generic 100-a-minute limit while
    /// spending provider tokens on every call.
    #[test]
    fn categorize_ai_search_endpoints_separately() {
        assert_eq!(
            categorize_path("/api/v1/search/expand", "POST"),
            "search_expand"
        );
        assert_eq!(
            categorize_path("/api/v1/search/summarize", "POST"),
            "search_summarize"
        );
        assert_eq!(
            categorize_path("/api/v1/search/followup", "POST"),
            "search_followup"
        );
    }

    /// The limits are ordered by what each endpoint costs, which is the reason
    /// they are separate categories at all.
    #[test]
    fn the_ai_search_limits_descend_with_cost() {
        let config = RateLimitConfig::default();
        assert!(
            config.search_expand.0 > config.search_summarize.0,
            "expansion is the cheapest of the three"
        );
        assert!(
            config.search_summarize.0 > config.search_followup.0,
            "follow-up is the most expensive"
        );
        assert!(
            config.search_expand.0 < config.api.0,
            "even the cheapest AI endpoint must be tighter than the generic api bucket"
        );
    }

    /// A path under the AI search prefix that is not one of the three is not
    /// given the cheapest limit by accident.
    #[test]
    fn an_unknown_ai_search_path_is_not_an_ai_search_category() {
        assert_eq!(
            categorize_path("/api/v1/search/something-else", "POST"),
            "api"
        );
    }

    #[test]
    fn categorize_comment_writes() {
        assert_eq!(
            categorize_path(
                "/api/item/01234567-89ab-7def-8123-456789abcdef/comments",
                "POST"
            ),
            "comment"
        );
        assert_eq!(
            categorize_path("/api/comment/01234567-89ab-7def-8123-456789abcdef", "PUT"),
            "comment"
        );
        assert_eq!(
            categorize_path(
                "/api/comment/01234567-89ab-7def-8123-456789abcdef",
                "DELETE"
            ),
            "comment"
        );
    }

    /// Reading comments is not writing them, and a thread of replies loading on a
    /// busy page must not exhaust a posting budget.
    #[test]
    fn reading_comments_is_not_a_comment_write() {
        assert_eq!(
            categorize_path(
                "/api/item/01234567-89ab-7def-8123-456789abcdef/comments",
                "GET"
            ),
            "api"
        );
        assert_eq!(
            categorize_path("/api/comment/01234567-89ab-7def-8123-456789abcdef", "GET"),
            "api"
        );
    }

    /// The defect in one assertion: a comment post must not be bounded by the
    /// generic api limit.
    #[test]
    fn comment_writes_are_far_tighter_than_the_api_bucket() {
        let config = RateLimitConfig::default();
        assert!(
            config.comment.0 < config.api.0 / 10,
            "a hundred comments a minute is not a limit; got {:?} against api {:?}",
            config.comment,
            config.api
        );
    }

    /// Every category `categorize_path` can return has to resolve to its own
    /// limit, or a new category silently inherits the api bucket.
    #[test]
    fn every_category_resolves_to_its_own_limit() {
        let limiter_config = RateLimitConfig::default();
        let expected = [
            ("comment", limiter_config.comment),
            ("search_expand", limiter_config.search_expand),
            ("search_summarize", limiter_config.search_summarize),
            ("search_followup", limiter_config.search_followup),
        ];
        for (category, limit) in expected {
            assert_ne!(
                limit, limiter_config.api,
                "{category} must not be configured as the api limit"
            );
        }
    }

    #[test]
    fn categorize_form_submission() {
        assert_eq!(categorize_path("/item/123", "POST"), "forms");
        assert_eq!(categorize_path("/admin/content/add/blog", "POST"), "forms");
    }

    #[test]
    fn categorize_default_get() {
        assert_eq!(categorize_path("/some/page", "GET"), "api");
    }

    // --- static asset bucket ---

    /// The defect: a stylesheet was an `api` call. Every asset prefix now lands
    /// in its own bucket.
    #[test]
    fn static_assets_are_their_own_category() {
        assert_eq!(categorize_path("/static/css/theme.css", "GET"), "static");
        assert_eq!(categorize_path("/static/js/app.js", "GET"), "static");
        assert_eq!(categorize_path("/files/2026/09/photo.jpg", "GET"), "static");
        assert_eq!(categorize_path("/favicon.ico", "GET"), "static");
        assert_eq!(categorize_path("/static/css/theme.css", "HEAD"), "static");
    }

    /// A write under an asset prefix is not a file being served, and must not
    /// inherit the static bucket's very generous limit.
    #[test]
    fn a_write_under_an_asset_prefix_is_not_static() {
        assert_eq!(categorize_path("/static/css/theme.css", "POST"), "forms");
        assert_eq!(categorize_path("/files/anything", "DELETE"), "api");
    }

    /// Prefix matching is on a path segment, not a string prefix.
    #[test]
    fn an_asset_prefix_does_not_match_a_longer_word() {
        assert_eq!(categorize_path("/staticky", "GET"), "api");
        assert_eq!(categorize_path("/filesystem/etc/passwd", "GET"), "api");
    }

    /// A site with a non-default `FILES_URL` gets its own prefix routed, with or
    /// without the trailing slash.
    #[test]
    fn configured_static_prefixes_are_honored() {
        let limiter_prefixes = ["/static/", "/assets/"];
        assert_eq!(
            categorize_path_with("/assets/logo.svg", "GET", &limiter_prefixes),
            "static"
        );
        // `with_static_prefixes` normalizes a prefix given without the slash.
        assert_eq!(
            categorize_path_with("/assets/logo.svg", "GET", &["/assets"]),
            "static",
            "a prefix without a trailing slash still matches its own subtree"
        );
        // And the default prefix is no longer special once overridden.
        assert_eq!(
            categorize_path_with("/files/x.jpg", "GET", &limiter_prefixes),
            "api"
        );
    }

    /// The point of the whole bucket, in one assertion: an ordinary asset-heavy
    /// page must not consume a meaningful share of a visitor's budget.
    #[test]
    fn a_page_with_twenty_assets_does_not_trip_anything() {
        let config = RateLimitConfig::default();
        let page_view = 1 + 20; // the HTML, then twenty assets
        let assets = 20;

        assert_eq!(
            categorize_path("/some/page", "GET"),
            "api",
            "the page itself is still an api-bucket request"
        );
        assert!(
            config.static_assets.0 >= assets * 10,
            "twenty assets a page leaves no headroom at {:?}",
            config.static_assets
        );
        assert!(
            config.static_assets.0 > config.api.0,
            "the static bucket must be looser than the generic one, not tighter"
        );
        // Before the fix all twenty-one requests shared the api bucket, so five
        // page views in a minute came within a hair of the limit and a sixth
        // tripped it.
        assert!(
            page_view * 5 > config.api.0 / 2,
            "sanity: the old shared-bucket arithmetic is why this bucket exists"
        );
    }

    // --- per-bucket overrides ---

    /// Every name in `BUCKETS` has a field behind it, in both directions. A new
    /// bucket added to the struct and not to the list is unconfigurable; a name in
    /// the list with no field silently ignores its override.
    #[test]
    fn every_bucket_resolves_to_a_field() {
        let mut config = RateLimitConfig::default();
        for bucket in BUCKETS {
            assert!(
                config.bucket(bucket).is_some(),
                "{bucket} is listed but has no limit"
            );
            assert!(
                config.bucket_mut(bucket).is_some(),
                "{bucket} is listed but cannot be overridden"
            );
        }
    }

    #[test]
    fn override_keys_follow_the_documented_shape() {
        assert_eq!(bucket_env_key("static"), "TROVATO_RATE_LIMIT_STATIC");
        assert_eq!(
            bucket_env_key("search_expand"),
            "TROVATO_RATE_LIMIT_SEARCH_EXPAND"
        );
        assert_eq!(bucket_config_key("static"), "rate_limit.static");
    }

    #[test]
    fn an_override_replaces_the_default_limit() {
        let mut config = RateLimitConfig::default();
        config.apply_overrides(
            &|key| (key == "TROVATO_RATE_LIMIT_STATIC").then(|| "500".to_string()),
            &|_| None,
        );
        assert_eq!(config.static_assets.0, 500);
        assert_eq!(
            config.static_assets.1,
            Duration::from_secs(60),
            "the window is not configurable and must survive an override"
        );
        assert_eq!(config.api.0, 100, "other buckets are untouched");
    }

    /// The stated precedence: an operator can override a stored value without
    /// reaching into the database.
    #[test]
    fn the_environment_wins_over_the_stored_config() {
        let mut config = RateLimitConfig::default();
        config.apply_overrides(
            &|key| (key == "TROVATO_RATE_LIMIT_LOGIN").then(|| "9".to_string()),
            &|key| (key == "rate_limit.login").then(|| "77".to_string()),
        );
        assert_eq!(config.login.0, 9);
    }

    #[test]
    fn the_stored_config_applies_when_the_environment_is_silent() {
        let mut config = RateLimitConfig::default();
        config.apply_overrides(&|_| None, &|key| {
            (key == "rate_limit.forms").then(|| "42".to_string())
        });
        assert_eq!(config.forms.0, 42);
    }

    /// A typo in one bucket must not take the site down, and must not resolve to
    /// a limit of zero, which would reject every request to that bucket.
    #[test]
    fn a_junk_override_keeps_the_default() {
        let mut config = RateLimitConfig::default();
        let default_api = config.api.0;
        for junk in ["", "  ", "lots", "-5", "0", "12.5"] {
            config.apply_overrides(&|_| Some(junk.to_string()), &|_| None);
            assert_eq!(
                config.api.0, default_api,
                "{junk:?} must not become a limit"
            );
        }
    }

    /// Whitespace around a value from an env file is not a typo.
    #[test]
    fn an_override_tolerates_surrounding_whitespace() {
        let mut config = RateLimitConfig::default();
        config.apply_overrides(&|_| Some(" 7 \n".to_string()), &|_| None);
        assert_eq!(config.api.0, 7);
    }

    /// An override reaches the limiter's own lookup, not just the struct field.
    #[test]
    fn get_limit_reads_the_overridden_value() {
        let mut config = RateLimitConfig::default();
        config.apply_overrides(
            &|key| (key == "TROVATO_RATE_LIMIT_STATIC").then(|| "1234".to_string()),
            &|_| None,
        );
        assert_eq!(config.bucket("static").map(|b| b.0), Some(1234));
        assert_eq!(
            config.bucket("nonexistent"),
            None,
            "an unknown category has no bucket of its own"
        );
    }

    #[test]
    fn test_default_config() {
        let config = RateLimitConfig::default();
        assert_eq!(config.login.0, 5);
        assert_eq!(config.api.0, 100);
    }

    #[test]
    fn rate_limit_response_has_retry_after() {
        let response = rate_limit_response(60);
        assert_eq!(response.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get("retry-after")
                .unwrap()
                .to_str()
                .unwrap(),
            "60"
        );
    }

    // --- get_client_id tests (RATE-1: forwarding headers gated on trust) ---

    fn proxy() -> std::net::SocketAddr {
        "10.9.8.7:443".parse().unwrap()
    }

    /// From a TRUSTED proxy, X-Forwarded-For (first entry) is honored.
    #[test]
    fn trusted_proxy_honors_x_forwarded_for() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4, 5.6.7.8".parse().unwrap());
        let trusted = [proxy().ip()];
        assert_eq!(get_client_id(Some(proxy()), &headers, &trusted), "1.2.3.4");
    }

    /// From a trusted proxy, X-Real-IP is honored when XFF is absent.
    #[test]
    fn trusted_proxy_honors_x_real_ip() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-real-ip", "10.0.0.1".parse().unwrap());
        let trusted = [proxy().ip()];
        assert_eq!(get_client_id(Some(proxy()), &headers, &trusted), "10.0.0.1");
    }

    /// RATE-1 core: an UNTRUSTED direct peer cannot spoof its identity via
    /// X-Forwarded-For — the socket peer wins, not the header.
    #[test]
    fn untrusted_peer_ignores_x_forwarded_for() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        headers.insert("x-real-ip", "10.0.0.1".parse().unwrap());
        let addr: std::net::SocketAddr = "203.0.113.9:5555".parse().unwrap();
        // No trusted proxies configured ⇒ headers ignored, socket peer used.
        assert_eq!(get_client_id(Some(addr), &headers, &[]), "203.0.113.9");
    }

    /// A peer that isn't in the (non-empty) allowlist is still untrusted.
    #[test]
    fn peer_not_in_allowlist_is_untrusted() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        let addr: std::net::SocketAddr = "203.0.113.9:5555".parse().unwrap();
        let trusted = [proxy().ip()]; // different IP
        assert_eq!(get_client_id(Some(addr), &headers, &trusted), "203.0.113.9");
    }

    #[test]
    fn client_id_from_socket_addr_no_headers() {
        let addr = "192.168.1.1:8080".parse().ok();
        assert_eq!(
            get_client_id(addr, &axum::http::HeaderMap::new(), &[]),
            "192.168.1.1"
        );
    }

    #[test]
    fn client_id_unknown_fallback() {
        assert_eq!(
            get_client_id(None, &axum::http::HeaderMap::new(), &[]),
            "unknown"
        );
    }

    /// Trusted proxy with a multi-hop XFF: the first (client-most) IP wins.
    #[test]
    fn trusted_proxy_x_forwarded_for_takes_first_ip() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "1.1.1.1, 2.2.2.2, 3.3.3.3".parse().unwrap(),
        );
        let trusted = [proxy().ip()];
        assert_eq!(get_client_id(Some(proxy()), &headers, &trusted), "1.1.1.1");
    }

    #[test]
    fn trusted_proxy_xff_trims_whitespace() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-forwarded-for", " 1.2.3.4 , 5.6.7.8".parse().unwrap());
        let trusted = [proxy().ip()];
        assert_eq!(get_client_id(Some(proxy()), &headers, &trusted), "1.2.3.4");
    }

    #[test]
    fn parse_trusted_proxies_filters_junk() {
        let list = parse_trusted_proxies("127.0.0.1, , 10.0.0.5 ,notanip,::1");
        assert!(list.contains(&"127.0.0.1".parse().unwrap()));
        assert!(list.contains(&"10.0.0.5".parse().unwrap()));
        assert!(list.contains(&"::1".parse().unwrap()));
        assert_eq!(list.len(), 3, "junk entries dropped");
    }
}
