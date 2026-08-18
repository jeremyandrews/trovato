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
            // Per docs/design/search-architecture.md §Rate Limiting.
            search_expand: (30, Duration::from_secs(60)), // 30 per minute
            search_summarize: (10, Duration::from_secs(60)), // 10 per minute
            search_followup: (5, Duration::from_secs(60)), // 5 per minute
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
}

impl RateLimiter {
    /// Create a new rate limiter.
    pub fn new(redis: RedisClient, config: RateLimitConfig, trusted_proxies: Vec<IpAddr>) -> Self {
        Self {
            redis,
            config,
            trusted_proxies: Arc::new(trusted_proxies),
        }
    }

    /// The configured trusted-proxy allowlist.
    pub fn trusted_proxies(&self) -> &[IpAddr] {
        &self.trusted_proxies
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
    fn get_limit(&self, category: &str) -> (u32, Duration) {
        match category {
            "login" => self.config.login,
            "forms" => self.config.forms,
            "api" => self.config.api,
            "search" => self.config.search,
            "uploads" => self.config.uploads,
            "register" => self.config.register,
            "verify_email" => self.config.verify_email,
            "profile" => self.config.profile,
            "password" => self.config.password,
            "recovery" => self.config.recovery,
            "comment" => self.config.comment,
            "search_expand" => self.config.search_expand,
            "search_summarize" => self.config.search_summarize,
            "search_followup" => self.config.search_followup,
            _ => self.config.api, // Default to API limits
        }
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

/// Categorize a request path for rate limiting.
///
/// Order matters: the specific categories are tested before the generic `/api/`
/// one, since every path they name is also an `/api/` path. That is how the AI
/// search endpoints and the comment writes used to land in the `api` bucket at
/// 100 a minute.
pub fn categorize_path(path: &str, method: &str) -> &'static str {
    if path.starts_with("/user/login") && method == "POST" {
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
    let category = categorize_path(request.uri().path(), request.method().as_str());
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
        let category = categorize_path(request.uri().path(), request.method().as_str());
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
