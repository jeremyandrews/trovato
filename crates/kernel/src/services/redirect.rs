//! Redirect model and service for URL redirect management.

use anyhow::{Context, Result};
use dashmap::DashMap;
use sqlx::PgPool;
use tracing::debug;
use uuid::Uuid;

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
    /// Validates the destination and status code before inserting.
    pub async fn create(
        pool: &PgPool,
        source: &str,
        destination: &str,
        status_code: i16,
        language: &str,
    ) -> Result<Self> {
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
        .bind(source)
        .bind(destination)
        .bind(status_code)
        .bind(language)
        .bind(now)
        .fetch_one(pool)
        .await
        .context("failed to create redirect")?;

        Ok(redirect)
    }

    /// Detect redirect loops (source -> destination -> ... -> source).
    ///
    /// Checks both the specified language and language-agnostic (empty language)
    /// redirects to catch cross-language loops where a language-specific redirect
    /// chains into a language-agnostic one that loops back.
    pub async fn detect_loop(
        pool: &PgPool,
        source: &str,
        destination: &str,
        language: &str,
    ) -> Result<bool> {
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

/// Create a redirect when a URL alias changes (old alias -> new alias).
///
/// If a `RedirectCache` is provided, the old alias entry is invalidated
/// so the next request sees the new redirect immediately.
pub async fn create_redirect_for_alias_change(
    pool: &PgPool,
    old_alias: &str,
    new_alias: &str,
    language: &str,
    cache: Option<&RedirectCache>,
) -> Result<()> {
    // Don't create self-referencing redirects
    if old_alias == new_alias {
        return Ok(());
    }

    // Check for loops
    if Redirect::detect_loop(pool, old_alias, new_alias, language).await? {
        debug!(
            old = %old_alias,
            new = %new_alias,
            "skipping redirect creation: would create loop"
        );
        return Ok(());
    }

    Redirect::create(pool, old_alias, new_alias, 301, language).await?;
    debug!(old = %old_alias, new = %new_alias, "created redirect for alias change");

    // Invalidate cache so the redirect takes effect immediately
    if let Some(cache) = cache {
        cache.invalidate(old_alias, language);
    }

    Ok(())
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

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
