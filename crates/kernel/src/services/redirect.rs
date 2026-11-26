//! Redirect model and service for URL redirect management.

use anyhow::{Context, Result};
use dashmap::DashMap;
use sqlx::PgPool;
use uuid::Uuid;

use crate::cache::generation::{CacheKind, bump};
use crate::services::audit::AuditTrail;

/// Cache TTL in seconds.
const CACHE_TTL_SECS: i64 = 60;

/// Maximum number of entries in the redirect cache (soft cap).
/// Prevents unbounded memory growth from attackers requesting unique paths.
/// This is an approximate limit — under concurrent load, the cache may
/// temporarily exceed this by a small amount due to DashMap's sharded len().
const MAX_CACHE_ENTRIES: usize = 10_000;

/// Cached redirect lookup result (including negative lookups).
#[derive(Clone)]
struct CachedEntry {
    redirect: Option<Redirect>,
    expires_at: i64,
}

/// In-memory redirect cache with TTL and size bounds.
///
/// Caches both hits and misses to avoid repeated DB queries for unknown paths.
/// Key is `(source, language)`.
pub struct RedirectCache {
    entries: DashMap<(String, String), CachedEntry>,
}

impl Default for RedirectCache {
    fn default() -> Self {
        Self::new()
    }
}

impl RedirectCache {
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
        }
    }

    /// Look up a redirect for a request: the exact path plus its normalized
    /// query first (when the request carries one), falling back to the path
    /// alone — the same precedence a path-only redirect already matched
    /// under, so a site with no query-bearing redirects sees no change
    /// (B164). Each step is an indexed exact-match lookup through [`find`](Self::find);
    /// never a scan.
    pub async fn find_for_request(
        &self,
        pool: &PgPool,
        path: &str,
        query: Option<&str>,
        language: &str,
    ) -> Result<Option<Redirect>> {
        let with_query = normalize_source(path, query);
        if with_query != path {
            let hit = self.find(pool, &with_query, language).await?;
            if hit.is_some() {
                return Ok(hit);
            }
        }
        self.find(pool, path, language).await
    }

    /// Look up a redirect, using the cache first, falling back to DB.
    ///
    /// Also tries a language-agnostic lookup (language = "") if the
    /// language-specific lookup misses.
    pub async fn find(
        &self,
        pool: &PgPool,
        source: &str,
        language: &str,
    ) -> Result<Option<Redirect>> {
        let now = chrono::Utc::now().timestamp();

        // Check cache for language-specific entry
        let key = (source.to_string(), language.to_string());
        if let Some(entry) = self.entries.get(&key) {
            if entry.expires_at > now {
                return Ok(entry.redirect.clone());
            }
            // Expired — drop the reference so we can remove + re-insert below
            drop(entry);
        }

        // Cache miss or expired — query DB
        let redirect = Redirect::find_by_source(pool, source, language).await?;

        // If no language-specific redirect, try language-agnostic fallback
        let redirect = match redirect {
            Some(r) => Some(r),
            None if !language.is_empty() => Redirect::find_by_source(pool, source, "").await?,
            None => None,
        };

        // Cache the result if under capacity
        if self.entries.len() < MAX_CACHE_ENTRIES {
            self.entries.insert(
                key,
                CachedEntry {
                    redirect: redirect.clone(),
                    expires_at: now + CACHE_TTL_SECS,
                },
            );
        } else {
            // Over capacity — evict expired entries, then try again
            self.evict_expired(now);
            if self.entries.len() < MAX_CACHE_ENTRIES {
                self.entries.insert(
                    key,
                    CachedEntry {
                        redirect: redirect.clone(),
                        expires_at: now + CACHE_TTL_SECS,
                    },
                );
            }
            // If still over capacity, skip caching (serve from DB)
        }

        Ok(redirect)
    }

    /// Remove all expired entries from the cache.
    fn evict_expired(&self, now: i64) {
        self.entries.retain(|_, entry| entry.expires_at > now);
    }

    /// Invalidate a specific cached redirect entry.
    ///
    /// Call this after creating, updating, or deleting a redirect.
    pub fn invalidate(&self, source: &str, language: &str) {
        self.entries
            .remove(&(source.to_string(), language.to_string()));
        // Also invalidate the language-agnostic entry
        self.entries.remove(&(source.to_string(), String::new()));
    }

    /// Clear the entire cache.
    pub fn clear(&self) {
        self.entries.clear();
    }
}

impl std::fmt::Debug for RedirectCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedirectCache")
            .field("entries", &self.entries.len())
            .finish()
    }
}

/// A URL redirect record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct Redirect {
    pub id: Uuid,
    pub source: String,
    pub destination: String,
    pub status_code: i16,
    pub language: String,
    pub created: i64,
}

/// Split a redirect source into its path and raw query string.
///
/// Rejects a fragment (`#...`): a redirect is matched against a request's
/// path and query only, so a source that names a fragment could never match
/// anything and almost certainly means the author pasted a full URL.
fn split_source(source: &str) -> Result<(&str, Option<&str>)> {
    if source.contains('#') {
        anyhow::bail!("redirect source must not include a fragment");
    }
    Ok(match source.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (source, None),
    })
}

/// Percent-decode one query key or value, treating `+` as a space the way a
/// form-encoded query does.
fn decode_component(raw: &str) -> String {
    let spaced = raw.replace('+', " ");
    urlencoding::decode(&spaced)
        .map(|decoded| decoded.into_owned())
        .unwrap_or(spaced)
}

/// Canonicalize a raw query string: each key/value percent-decoded then
/// re-encoded, and the pairs sorted, so two sources that differ only in
/// parameter order or percent-encoding compare equal.
///
/// `None` for an absent or empty query, so a bare path round-trips with no
/// trailing `?`.
fn normalize_query(query: Option<&str>) -> Option<String> {
    let query = query.filter(|q| !q.is_empty())?;
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode_component(key), decode_component(value))
        })
        .collect();
    if pairs.is_empty() {
        return None;
    }
    pairs.sort();
    Some(
        pairs
            .into_iter()
            .map(|(key, value)| {
                format!(
                    "{}={}",
                    urlencoding::encode(&key),
                    urlencoding::encode(&value)
                )
            })
            .collect::<Vec<_>>()
            .join("&"),
    )
}

/// The canonical source a redirect is stored and matched under: `path` alone
/// when `query` is absent or empty, else `path?normalized_query` with its
/// parameters sorted and percent-encoding canonical (B164).
pub fn normalize_source(path: &str, query: Option<&str>) -> String {
    match normalize_query(query) {
        Some(q) => format!("{path}?{q}"),
        None => path.to_string(),
    }
}

/// Normalize a redirect source as entered (admin form, API, or any other
/// writer): split off its query, reject a fragment, and canonicalize.
pub fn canonicalize_source(source: &str) -> Result<String> {
    let (path, query) = split_source(source)?;
    Ok(normalize_source(path, query))
}

/// Validate that a redirect destination is safe.
///
/// Permits:
/// - Relative paths starting with `/`
/// - Absolute URLs with `http://` or `https://` schemes
///
/// Rejects:
/// - `javascript:`, `data:`, `vbscript:`, and other dangerous schemes
/// - Empty destinations
pub fn validate_redirect_destination(destination: &str) -> bool {
    if destination.is_empty() {
        return false;
    }
    // Relative paths are always safe
    if destination.starts_with('/') && !destination.starts_with("//") {
        return true;
    }
    // Absolute URLs must use http or https
    if destination.starts_with("https://") || destination.starts_with("http://") {
        return true;
    }
    false
}

impl Redirect {
    /// Find a redirect by source path.
    pub async fn find_by_source(
        pool: &PgPool,
        source: &str,
        language: &str,
    ) -> Result<Option<Self>> {
        let redirect = sqlx::query_as::<_, Redirect>(
            r#"
            SELECT id, source, destination, status_code, language, created
            FROM redirect
            WHERE source = $1 AND language = $2
            ORDER BY created DESC
            LIMIT 1
            "#,
        )
        .bind(source)
        .bind(language)
        .fetch_optional(pool)
        .await
        .context("failed to find redirect by source")?;

        Ok(redirect)
    }

    /// Create a new redirect.
    ///
    /// `source` is canonicalized before it is stored (B164): its query, if
    /// any, has its parameters sorted and its percent-encoding made
    /// canonical, so a request matches it regardless of parameter order or
    /// encoding. A source with a fragment, or an unsafe destination or
    /// status code, is refused.
    pub async fn create(
        pool: &PgPool,
        source: &str,
        destination: &str,
        status_code: i16,
        language: &str,
    ) -> Result<Self> {
        let source = canonicalize_source(source)?;

        if !validate_redirect_destination(destination) {
            anyhow::bail!("invalid redirect destination: must be a relative path or http(s) URL");
        }

        // Only allow valid HTTP redirect status codes
        if !matches!(status_code, 301 | 302 | 303 | 307 | 308) {
            anyhow::bail!(
                "invalid redirect status code {status_code}: must be 301, 302, 303, 307, or 308"
            );
        }

        let id = Uuid::now_v7();
        let now = chrono::Utc::now().timestamp();

        let redirect = sqlx::query_as::<_, Redirect>(
            r#"
            INSERT INTO redirect (id, source, destination, status_code, language, created)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, source, destination, status_code, language, created
            "#,
        )
        .bind(id)
        .bind(&source)
        .bind(destination)
        .bind(status_code)
        .bind(language)
        .bind(now)
        .fetch_one(pool)
        .await
        .context("failed to create redirect")?;

        Ok(redirect)
    }

    /// Update an existing redirect's source, destination, status code and
    /// language. `source` is canonicalized exactly as [`create`](Self::create) does.
    ///
    /// Returns `Ok(None)` when `id` does not name a redirect.
    pub async fn update(
        pool: &PgPool,
        id: Uuid,
        source: &str,
        destination: &str,
        status_code: i16,
        language: &str,
    ) -> Result<Option<Self>> {
        let source = canonicalize_source(source)?;

        if !validate_redirect_destination(destination) {
            anyhow::bail!("invalid redirect destination: must be a relative path or http(s) URL");
        }

        if !matches!(status_code, 301 | 302 | 303 | 307 | 308) {
            anyhow::bail!(
                "invalid redirect status code {status_code}: must be 301, 302, 303, 307, or 308"
            );
        }

        let redirect = sqlx::query_as::<_, Redirect>(
            r#"
            UPDATE redirect
            SET source = $2, destination = $3, status_code = $4, language = $5
            WHERE id = $1
            RETURNING id, source, destination, status_code, language, created
            "#,
        )
        .bind(id)
        .bind(&source)
        .bind(destination)
        .bind(status_code)
        .bind(language)
        .fetch_optional(pool)
        .await
        .context("failed to update redirect")?;

        Ok(redirect)
    }

    /// Find a redirect by id.
    pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Self>> {
        let redirect = sqlx::query_as::<_, Redirect>(
            r#"
            SELECT id, source, destination, status_code, language, created
            FROM redirect
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to find redirect by id")?;

        Ok(redirect)
    }

    /// Total number of redirects, for pagination.
    pub async fn count_all(pool: &PgPool) -> Result<i64> {
        let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM redirect")
            .fetch_one(pool)
            .await
            .context("failed to count redirects")?;

        Ok(count)
    }

    /// Detect redirect loops (source -> destination -> ... -> source).
    ///
    /// Checks both the specified language and language-agnostic (empty language)
    /// redirects to catch cross-language loops where a language-specific redirect
    /// chains into a language-agnostic one that loops back.
    ///
    /// A destination that is its own source is a loop of zero hops, caught
    /// here before the chain walk: the walk below only ever compares a
    /// *later* hop's destination against `source`, so a bare self-redirect
    /// (no other rows involved at all) would otherwise pass.
    pub async fn detect_loop(
        pool: &PgPool,
        source: &str,
        destination: &str,
        language: &str,
    ) -> Result<bool> {
        if destination == source {
            return Ok(true);
        }

        // Follow the chain from destination to see if it leads back to source
        let mut current = destination.to_string();
        let mut depth = 0;
        const MAX_DEPTH: u32 = 10;

        while depth < MAX_DEPTH {
            // Check both language-specific and language-agnostic redirects
            let next = sqlx::query_scalar::<_, String>(
                "SELECT destination FROM redirect WHERE source = $1 AND (language = $2 OR language = '') ORDER BY language DESC LIMIT 1",
            )
            .bind(&current)
            .bind(language)
            .fetch_optional(pool)
            .await
            .context("failed to check redirect chain")?;

            match next {
                Some(dest) if dest == source => return Ok(true),
                Some(dest) => {
                    current = dest;
                    depth += 1;
                }
                None => return Ok(false),
            }
        }

        // If we hit max depth, assume potential loop
        Ok(true)
    }

    /// List all redirects with pagination.
    pub async fn list_all(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<Self>> {
        let redirects = sqlx::query_as::<_, Redirect>(
            r#"
            SELECT id, source, destination, status_code, language, created
            FROM redirect
            ORDER BY created DESC
            LIMIT $1 OFFSET $2
            "#,
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .context("failed to list redirects")?;

        Ok(redirects)
    }

    /// Delete a redirect.
    pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool> {
        let result = sqlx::query("DELETE FROM redirect WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to delete redirect")?;

        Ok(result.rows_affected() > 0)
    }
}

/// The one write path for a redirect: the admin form, the JSON API, and any
/// other caller all go through here, so none of them can skip the loop
/// check, the cache bump, or the audit entry. The B167 importer does not:
/// it keeps the id and `created` a record gives it (this service always
/// assigns its own) and writes inside its own transaction, bumping once for
/// the whole run instead of once per record (`import::alias`).
#[derive(Clone)]
pub struct RedirectService {
    pool: PgPool,
    audit: AuditTrail,
    cache: Option<std::sync::Arc<RedirectCache>>,
}

impl RedirectService {
    /// Create the service. `cache` is this server's own redirect lookup
    /// cache, cleared at once on a successful write; `None` when the
    /// `trovato_redirects` plugin was not enabled at startup.
    pub fn new(
        pool: PgPool,
        audit: AuditTrail,
        cache: Option<std::sync::Arc<RedirectCache>>,
    ) -> Self {
        Self { pool, audit, cache }
    }

    /// Clear this server's cache at once and tell every other server to drop
    /// theirs too. Call after a write commits.
    fn invalidate(&self) {
        if let Some(cache) = &self.cache {
            cache.clear();
        }
        bump(CacheKind::Redirects);
    }

    /// Record one redirect write in the audit log.
    async fn audit(&self, action: &str, redirect: &Redirect) {
        self.audit
            .record(
                action,
                "redirect",
                redirect.id,
                None,
                serde_json::json!({
                    "source": redirect.source,
                    "destination": redirect.destination,
                }),
            )
            .await;
    }

    /// Create a redirect. Refused, with nothing written, when `source`
    /// (canonicalized the way [`Redirect::create`] stores it) and
    /// `destination` would form a loop, including a destination that is its
    /// own source.
    pub async fn create(
        &self,
        source: &str,
        destination: &str,
        status_code: i16,
        language: &str,
    ) -> Result<Redirect> {
        let canonical_source = canonicalize_source(source)?;
        if Redirect::detect_loop(&self.pool, &canonical_source, destination, language).await? {
            anyhow::bail!(
                "redirect refused: {canonical_source} -> {destination} would create a redirect loop"
            );
        }

        let redirect = Redirect::create(&self.pool, source, destination, status_code, language)
            .await
            .context("failed to create redirect")?;
        self.invalidate();
        self.audit("create", &redirect).await;
        Ok(redirect)
    }

    /// Update a redirect's source, destination, status code and language.
    /// `Ok(None)` when `id` does not name a redirect. Refused, with nothing
    /// written, on the same loop check as [`create`](Self::create).
    pub async fn update(
        &self,
        id: Uuid,
        source: &str,
        destination: &str,
        status_code: i16,
        language: &str,
    ) -> Result<Option<Redirect>> {
        let canonical_source = canonicalize_source(source)?;
        if Redirect::detect_loop(&self.pool, &canonical_source, destination, language).await? {
            anyhow::bail!(
                "redirect refused: {canonical_source} -> {destination} would create a redirect loop"
            );
        }

        let redirect = Redirect::update(&self.pool, id, source, destination, status_code, language)
            .await
            .context("failed to update redirect")?;
        if let Some(redirect) = &redirect {
            self.invalidate();
            self.audit("update", redirect).await;
        }
        Ok(redirect)
    }

    /// Delete a redirect. `false` when there is no such redirect.
    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        let redirect = Redirect::find_by_id(&self.pool, id).await?;
        let deleted = Redirect::delete(&self.pool, id).await?;
        if deleted {
            self.invalidate();
            if let Some(redirect) = redirect {
                self.audit("delete", &redirect).await;
            }
        }
        Ok(deleted)
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // Query normalization (B164)
    // -------------------------------------------------------------------------

    #[test]
    fn a_bare_path_normalizes_to_itself() {
        assert_eq!(normalize_source("/article.php", None), "/article.php");
        assert_eq!(normalize_source("/article.php", Some("")), "/article.php");
    }

    #[test]
    fn a_query_source_sorts_its_parameters() {
        assert_eq!(
            normalize_source("/article.php", Some("sid=1")),
            "/article.php?sid=1"
        );
        assert_eq!(
            normalize_source("/search", Some("b=2&a=1")),
            normalize_source("/search", Some("a=1&b=2")),
        );
        assert_eq!(
            normalize_source("/search", Some("b=2&a=1")),
            "/search?a=1&b=2"
        );
    }

    #[test]
    fn percent_encoding_and_plus_normalize_the_same_way() {
        let encoded = normalize_source("/search", Some("q=a%20b"));
        let plussed = normalize_source("/search", Some("q=a+b"));
        let literal = normalize_source("/search", Some("q=a b"));
        assert_eq!(encoded, plussed);
        assert_eq!(encoded, literal);
        assert_eq!(encoded, "/search?q=a%20b");
    }

    #[test]
    fn canonicalize_source_splits_path_and_query() {
        assert_eq!(
            canonicalize_source("/article.php?sid=1").unwrap(),
            "/article.php?sid=1"
        );
        assert_eq!(canonicalize_source("/old").unwrap(), "/old");
    }

    #[test]
    fn canonicalize_source_rejects_a_fragment() {
        assert!(canonicalize_source("/old#top").is_err());
        assert!(canonicalize_source("/old?page=2#top").is_err());
    }

    #[test]
    fn redirect_serialization() {
        let r = Redirect {
            id: Uuid::nil(),
            source: "/old".to_string(),
            destination: "/new".to_string(),
            status_code: 301,
            language: "en".to_string(),
            created: 0,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("/old"));
    }

    #[test]
    fn cache_stores_and_retrieves() {
        let cache = RedirectCache::new();
        let now = chrono::Utc::now().timestamp();

        let redirect = Redirect {
            id: Uuid::nil(),
            source: "/old".to_string(),
            destination: "/new".to_string(),
            status_code: 301,
            language: "en".to_string(),
            created: 0,
        };

        cache.entries.insert(
            ("/old".to_string(), "en".to_string()),
            CachedEntry {
                redirect: Some(redirect),
                expires_at: now + 60,
            },
        );

        let entry = cache
            .entries
            .get(&("/old".to_string(), "en".to_string()))
            .unwrap();
        assert!(entry.redirect.is_some());
        assert_eq!(entry.redirect.as_ref().unwrap().destination, "/new");
    }

    #[test]
    fn cache_eviction_removes_expired() {
        let cache = RedirectCache::new();
        let now = chrono::Utc::now().timestamp();

        // Insert expired entry
        cache.entries.insert(
            ("/expired".to_string(), "en".to_string()),
            CachedEntry {
                redirect: None,
                expires_at: now - 10,
            },
        );

        // Insert valid entry
        cache.entries.insert(
            ("/valid".to_string(), "en".to_string()),
            CachedEntry {
                redirect: None,
                expires_at: now + 60,
            },
        );

        assert_eq!(cache.entries.len(), 2);
        cache.evict_expired(now);
        assert_eq!(cache.entries.len(), 1);
        assert!(
            cache
                .entries
                .get(&("/valid".to_string(), "en".to_string()))
                .is_some()
        );
    }

    #[test]
    fn cache_invalidation() {
        let cache = RedirectCache::new();
        let now = chrono::Utc::now().timestamp();

        cache.entries.insert(
            ("/path".to_string(), "en".to_string()),
            CachedEntry {
                redirect: None,
                expires_at: now + 60,
            },
        );
        cache.entries.insert(
            ("/path".to_string(), String::new()),
            CachedEntry {
                redirect: None,
                expires_at: now + 60,
            },
        );

        assert_eq!(cache.entries.len(), 2);
        cache.invalidate("/path", "en");
        assert_eq!(cache.entries.len(), 0);
    }

    #[test]
    fn status_code_validation() {
        // Valid redirect status codes (can't test via Redirect::create without DB,
        // so test the matches! expression directly)
        for code in [301_i16, 302, 303, 307, 308] {
            assert!(matches!(code, 301 | 302 | 303 | 307 | 308));
        }
        // Invalid codes
        for code in [200_i16, 404, 500, 0, -1, 100] {
            assert!(!matches!(code, 301 | 302 | 303 | 307 | 308));
        }
    }

    #[test]
    fn destination_validation() {
        // Valid destinations
        assert!(validate_redirect_destination("/new-page"));
        assert!(validate_redirect_destination("/admin/content"));
        assert!(validate_redirect_destination("https://example.com/page"));
        assert!(validate_redirect_destination("http://example.com/page"));

        // Invalid destinations
        assert!(!validate_redirect_destination(""));
        assert!(!validate_redirect_destination("javascript:alert(1)"));
        assert!(!validate_redirect_destination(
            "data:text/html,<h1>pwned</h1>"
        ));
        assert!(!validate_redirect_destination("//evil.com/phish"));
        assert!(!validate_redirect_destination("vbscript:something"));
        assert!(!validate_redirect_destination("ftp://files.example.com"));
    }
}
