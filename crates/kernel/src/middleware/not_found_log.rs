//! The 404 log: every address a visitor requested that the site could not
//! find, aggregated per `(path, language)` in `not_found_log` (B306), for
//! `/admin/reports/page-not-found`.
//!
//! [`record_not_found`] is a middleware layer placed directly outside
//! [`crate::middleware::check_redirect`], inside
//! [`crate::middleware::negotiate_language`]: it sees exactly the path and
//! language a redirect would have matched. It never waits on the database: a
//! hit is handed to a bounded channel with `try_send` and dropped if the
//! channel is full, the same shape as [`crate::system_log`]'s writer. A
//! background task drains the channel in batches, coalesces each batch by
//! `(path, language)` in memory, and writes it in one statement.
//!
//! No IP address, user id, user agent or query string is ever stored.

use std::collections::BTreeMap;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use sqlx::PgPool;
use tokio::sync::mpsc;

use crate::middleware::language::ResolvedLanguage;
use crate::middleware::redirect::is_system_path;
use crate::state::AppState;

/// Rows waiting for the writer before the layer starts dropping them.
const CHANNEL_CAPACITY: usize = 2048;

/// Rows the writer drains in one batch.
const BATCH: usize = 200;

/// The longest path or referrer kept, in characters.
pub const MAX_LENGTH: usize = 2048;

/// The `site_config` key the settings are stored under.
pub const SETTINGS_KEY: &str = "not_found_log";

/// Default retention window, in days.
pub const DEFAULT_RETENTION_DAYS: i64 = 90;

/// Default row cap.
pub const DEFAULT_MAX_ROWS: i64 = 10_000;

/// Case-insensitive substrings an address is never recorded under, one per
/// line in the settings form.
pub const DEFAULT_IGNORE: &[&str] = &[
    "wp-admin",
    "wp-login",
    "wp-content",
    "wp-includes",
    "xmlrpc",
    ".php",
    ".asp",
    ".env",
    ".git",
    "cgi-bin",
];

fn truncate(mut s: String, max: usize) -> String {
    if let Some((at, _)) = s.char_indices().nth(max) {
        s.truncate(at);
    }
    s
}

/// A single 404, as the layer records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotFoundHit {
    pub path: String,
    pub language: String,
    pub referrer: Option<String>,
    pub when: i64,
}

/// One row the writer upserts, after coalescing a batch by `(path, language)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoalescedHit {
    pub path: String,
    pub language: String,
    pub referrer: Option<String>,
    pub hits: i64,
    pub first_seen: i64,
    pub last_seen: i64,
}

/// The settings the report's settings form edits, held in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotFoundLogSettings {
    pub enabled: bool,
    pub retention_days: i64,
    pub max_rows: i64,
    /// Case-insensitive substrings; a matching path is never recorded.
    pub ignore: Vec<String>,
}

impl Default for NotFoundLogSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: DEFAULT_RETENTION_DAYS,
            max_rows: DEFAULT_MAX_ROWS,
            ignore: DEFAULT_IGNORE.iter().map(|s| (*s).to_string()).collect(),
        }
    }
}

impl NotFoundLogSettings {
    /// Read the settings out of the `site_config` value they are stored
    /// under. Anything missing or the wrong shape falls back to the default.
    pub fn from_value(value: Option<&serde_json::Value>) -> Self {
        let defaults = Self::default();
        let Some(value) = value else {
            return defaults;
        };
        let enabled = value
            .get("enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(defaults.enabled);
        let retention_days = value
            .get("retention_days")
            .and_then(serde_json::Value::as_i64)
            .filter(|n| *n > 0)
            .unwrap_or(defaults.retention_days);
        let max_rows = value
            .get("max_rows")
            .and_then(serde_json::Value::as_i64)
            .filter(|n| *n > 0)
            .unwrap_or(defaults.max_rows);
        let ignore = value
            .get("ignore")
            .and_then(serde_json::Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or(defaults.ignore);
        Self {
            enabled,
            retention_days,
            max_rows,
            ignore,
        }
    }

    /// The settings as one `site_config` JSON value.
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.enabled,
            "retention_days": self.retention_days,
            "max_rows": self.max_rows,
            "ignore": self.ignore,
        })
    }

    /// Load the settings from the database, or the default if unset.
    pub async fn load(pool: &PgPool) -> Self {
        match crate::models::SiteConfig::get(pool, SETTINGS_KEY).await {
            Ok(value) => Self::from_value(value.as_ref()),
            Err(e) => {
                tracing::warn!(error = %e, "failed to read the 404 log settings, using the default");
                Self::default()
            }
        }
    }
}

/// Whether `path` matches one of `ignore`'s case-insensitive substrings.
pub fn is_ignored(path: &str, ignore: &[String]) -> bool {
    let lower = path.to_ascii_lowercase();
    ignore
        .iter()
        .any(|pattern| !pattern.is_empty() && lower.contains(&pattern.to_ascii_lowercase()))
}

/// `referrer` with its query string and fragment cut, truncated to
/// [`MAX_LENGTH`] characters. `None` for an unparsable or empty value.
pub fn clean_referrer(referrer: &str) -> Option<String> {
    let trimmed = referrer.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_fragment = trimmed.split('#').next().unwrap_or(trimmed);
    let without_query = without_fragment.split('?').next().unwrap_or(without_fragment);
    if without_query.is_empty() {
        return None;
    }
    Some(truncate(without_query.to_string(), MAX_LENGTH))
}

/// Coalesce a batch of hits by `(path, language)`: one row per key, with
/// `hits` the count, `first_seen`/`last_seen` the earliest/latest `when`, and
/// `referrer` the one on the hit with the latest `when` (ties keep the first
/// one seen, which is the order the channel delivered them in).
pub fn coalesce(hits: &[NotFoundHit]) -> Vec<CoalescedHit> {
    let mut rows: BTreeMap<(String, String), CoalescedHit> = BTreeMap::new();
    for hit in hits {
        let key = (hit.path.clone(), hit.language.clone());
        rows.entry(key)
            .and_modify(|row| {
                row.hits += 1;
                row.first_seen = row.first_seen.min(hit.when);
                if hit.when > row.last_seen {
                    row.last_seen = hit.when;
                    row.referrer = hit.referrer.clone();
                }
            })
            .or_insert_with(|| CoalescedHit {
                path: hit.path.clone(),
                language: hit.language.clone(),
                referrer: hit.referrer.clone(),
                hits: 1,
                first_seen: hit.when,
                last_seen: hit.when,
            });
    }
    rows.into_values().collect()
}

/// Write `rows` in one statement: an insert for a path and language not seen
/// before, an update (hits added, `last_seen` and `referrer` from the newest
/// hit, `first_seen` kept) for one already there.
pub async fn insert_batch(pool: &PgPool, rows: &[CoalescedHit]) -> Result<(), sqlx::Error> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut paths = Vec::with_capacity(rows.len());
    let mut languages = Vec::with_capacity(rows.len());
    let mut referrers = Vec::with_capacity(rows.len());
    let mut hits = Vec::with_capacity(rows.len());
    let mut first_seens = Vec::with_capacity(rows.len());
    let mut last_seens = Vec::with_capacity(rows.len());
    for row in rows {
        paths.push(row.path.clone());
        languages.push(row.language.clone());
        referrers.push(row.referrer.clone());
        hits.push(row.hits);
        first_seens.push(row.first_seen);
        last_seens.push(row.last_seen);
    }
    sqlx::query(
        "INSERT INTO not_found_log (path, language, referrer, hits, first_seen, last_seen) \
         SELECT * FROM UNNEST($1::varchar[], $2::varchar[], $3::varchar[], $4::bigint[], \
         $5::bigint[], $6::bigint[]) \
         ON CONFLICT (path, language) DO UPDATE SET \
            hits = not_found_log.hits + EXCLUDED.hits, \
            last_seen = GREATEST(not_found_log.last_seen, EXCLUDED.last_seen), \
            referrer = CASE WHEN EXCLUDED.last_seen >= not_found_log.last_seen \
                            THEN EXCLUDED.referrer ELSE not_found_log.referrer END, \
            first_seen = LEAST(not_found_log.first_seen, EXCLUDED.first_seen)",
    )
    .bind(&paths)
    .bind(&languages)
    .bind(&referrers)
    .bind(&hits)
    .bind(&first_seens)
    .bind(&last_seens)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete rows older than `days` (by `last_seen`), then all but the `max_rows`
/// highest by hits. Returns how many rows went.
pub async fn prune(pool: &PgPool, days: i64, max_rows: i64) -> Result<u64, sqlx::Error> {
    let cutoff = chrono::Utc::now().timestamp() - days.max(0) * 86_400;
    let by_age = sqlx::query("DELETE FROM not_found_log WHERE last_seen < $1")
        .bind(cutoff)
        .execute(pool)
        .await?
        .rows_affected();
    let by_count = sqlx::query(
        "DELETE FROM not_found_log WHERE ctid NOT IN \
         (SELECT ctid FROM not_found_log ORDER BY hits DESC LIMIT $1)",
    )
    .bind(max_rows.max(0))
    .execute(pool)
    .await?
    .rows_affected();
    Ok(by_age + by_count)
}

/// Delete rows the 404 log no longer needs to show: a path that now resolves
/// as a Live URL alias in its language, an existing item's `/item/<uuid>`, or
/// (only when `redirects_enabled`) a redirect source in its language.
pub async fn clear_resolved(pool: &PgPool, redirects_enabled: bool) -> Result<u64, sqlx::Error> {
    let mut removed = sqlx::query(
        "DELETE FROM not_found_log n WHERE EXISTS ( \
            SELECT 1 FROM url_alias a \
            WHERE a.stage_id = 'live' AND a.alias = n.path AND a.language = n.language)",
    )
    .execute(pool)
    .await?
    .rows_affected();

    removed += sqlx::query(
        "DELETE FROM not_found_log n WHERE n.path ~ '^/item/[0-9a-fA-F-]{36}$' AND EXISTS ( \
            SELECT 1 FROM item i WHERE i.id::text = substring(n.path from 7))",
    )
    .execute(pool)
    .await?
    .rows_affected();

    if redirects_enabled {
        removed += sqlx::query(
            "DELETE FROM not_found_log n WHERE EXISTS ( \
                SELECT 1 FROM redirect r WHERE r.source = n.path AND r.language = n.language)",
        )
        .execute(pool)
        .await?
        .rows_affected();
    }

    Ok(removed)
}

/// One row as the report shows it.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct NotFoundLogEntry {
    pub path: String,
    pub language: String,
    pub referrer: Option<String>,
    pub hits: i64,
    pub first_seen: i64,
    pub last_seen: i64,
}

/// One page of entries and the total row count, sorted by hits descending
/// unless `order_by` says otherwise. `order_by` must come from the report's
/// own allowed columns, never from a request.
pub async fn list(
    pool: &PgPool,
    order_by: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<(Vec<NotFoundLogEntry>, i64), sqlx::Error> {
    let order_by = order_by.unwrap_or("hits DESC");
    let list_sql = format!(
        "SELECT path, language, referrer, hits, first_seen, last_seen FROM not_found_log \
         ORDER BY {order_by}, last_seen DESC LIMIT $1 OFFSET $2"
    );
    let entries = sqlx::query_as::<_, NotFoundLogEntry>(sqlx::AssertSqlSafe(list_sql))
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM not_found_log")
        .fetch_one(pool)
        .await?;
    Ok((entries, total))
}

/// Delete the entry for one source and language: called after a redirect is
/// created or updated for it, so the report does not keep showing an address
/// that already has a redirect.
pub async fn delete_one(pool: &PgPool, path: &str, language: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM not_found_log WHERE path = $1 AND language = $2")
        .bind(path)
        .bind(language)
        .execute(pool)
        .await?;
    Ok(())
}

/// The channel, its receiver, and the settings this process holds in memory.
///
/// Loaded once, like [`crate::middleware::language::LanguageSet`]: the
/// request path only ever reads `settings()` (a lock and a clone of an
/// `Arc`), never the database, so a flood of 404s never costs a query.
pub struct NotFoundLogState {
    sender: mpsc::Sender<NotFoundHit>,
    receiver: tokio::sync::Mutex<Option<mpsc::Receiver<NotFoundHit>>>,
    settings: parking_lot::RwLock<std::sync::Arc<NotFoundLogSettings>>,
}

impl NotFoundLogState {
    pub fn new(settings: NotFoundLogSettings) -> Self {
        let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
        Self {
            sender,
            receiver: tokio::sync::Mutex::new(Some(receiver)),
            settings: parking_lot::RwLock::new(std::sync::Arc::new(settings)),
        }
    }

    /// The settings this server last loaded.
    pub fn settings(&self) -> std::sync::Arc<NotFoundLogSettings> {
        self.settings.read().clone()
    }

    /// Read the settings again from the database and serve them from the
    /// next request: what a save on the settings form, here or on another
    /// server, does.
    pub async fn reload_settings(&self, pool: &PgPool) {
        let settings = NotFoundLogSettings::load(pool).await;
        *self.settings.write() = std::sync::Arc::new(settings);
    }

    /// Hand a hit to the channel. Never waits: a full channel drops it.
    pub fn record(&self, hit: NotFoundHit) {
        let _ = self.sender.try_send(hit);
    }

    /// Drain the channel into `not_found_log` until the sender side is gone.
    /// Called once, at startup.
    pub async fn spawn_writer(&self, pool: PgPool) {
        let Some(receiver) = self.receiver.lock().await.take() else {
            return;
        };
        tokio::spawn(async move {
            let mut receiver = receiver;
            let mut batch = Vec::with_capacity(BATCH);
            while receiver.recv_many(&mut batch, BATCH).await > 0 {
                let rows = coalesce(&batch);
                if let Err(e) = insert_batch(&pool, &rows).await {
                    tracing::debug!(error = %e, rows = rows.len(), "404 log write failed");
                }
                batch.clear();
            }
        });
    }

    /// Test-only: drain whatever is queued right now, with no wait for a
    /// batch to fill or for a background task to run, and write it.
    /// Returns how many distinct `(path, language)` rows were written.
    pub async fn flush_for_test(&self, pool: &PgPool) -> usize {
        let mut guard = self.receiver.lock().await;
        let Some(receiver) = guard.as_mut() else {
            return 0;
        };
        let mut batch = Vec::new();
        while let Ok(hit) = receiver.try_recv() {
            batch.push(hit);
        }
        let rows = coalesce(&batch);
        let count = rows.len();
        if let Err(e) = insert_batch(pool, &rows).await {
            tracing::debug!(error = %e, "404 log test flush failed");
        }
        count
    }
}

/// The middleware layer. Placed directly outside
/// [`crate::middleware::check_redirect`], inside
/// [`crate::middleware::negotiate_language`].
pub async fn record_not_found(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let language = request
        .extensions()
        .get::<ResolvedLanguage>()
        .map(|l| l.0.clone());
    let referrer = request
        .headers()
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(clean_referrer);

    let response = next.run(request).await;
    let settings = state.not_found_log().settings();

    if should_record(response.status(), &method, &path, &settings) {
        let language = language.unwrap_or_else(|| state.default_language());
        state.not_found_log().record(NotFoundHit {
            path: truncate(path, MAX_LENGTH),
            language,
            referrer,
            when: chrono::Utc::now().timestamp(),
        });
    }

    response
}

/// Whether a response for `method` to `path`, with final `status`, is one the
/// 404 log keeps: a `404` to a `GET` or `HEAD`, the log turned on, the path
/// neither a system path nor on the ignore list.
pub fn should_record(
    status: StatusCode,
    method: &Method,
    path: &str,
    settings: &NotFoundLogSettings,
) -> bool {
    status == StatusCode::NOT_FOUND
        && matches!(*method, Method::GET | Method::HEAD)
        && settings.enabled
        && !is_system_path(path)
        && !is_ignored(path, &settings.ignore)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn hit(path: &str, language: &str, referrer: Option<&str>, when: i64) -> NotFoundHit {
        NotFoundHit {
            path: path.to_string(),
            language: language.to_string(),
            referrer: referrer.map(str::to_string),
            when,
        }
    }

    #[test]
    fn coalesce_counts_hits_and_keeps_the_newest_referrer() {
        let hits = vec![
            hit("/old", "en", Some("https://a.example/one"), 100),
            hit("/old", "en", Some("https://a.example/two"), 200),
            hit("/old", "en", None, 150),
            hit("/other", "en", None, 50),
        ];
        let rows = coalesce(&hits);
        assert_eq!(rows.len(), 2);
        let old = rows.iter().find(|r| r.path == "/old").unwrap();
        assert_eq!(old.hits, 3);
        assert_eq!(old.first_seen, 100);
        assert_eq!(old.last_seen, 200);
        assert_eq!(old.referrer.as_deref(), Some("https://a.example/two"));
    }

    #[test]
    fn coalesce_keys_on_path_and_language() {
        let hits = vec![hit("/x", "en", None, 1), hit("/x", "fr", None, 2)];
        let rows = coalesce(&hits);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn referrer_is_cut_to_its_path() {
        assert_eq!(
            clean_referrer("https://example.com/page?x=1&y=2#top"),
            Some("https://example.com/page".to_string())
        );
        assert_eq!(clean_referrer(""), None);
        assert_eq!(clean_referrer("   "), None);
        assert_eq!(
            clean_referrer("https://example.com/page"),
            Some("https://example.com/page".to_string())
        );
    }

    #[test]
    fn long_values_are_truncated() {
        let long = "/".to_string() + &"x".repeat(MAX_LENGTH + 50);
        let cut = truncate(long, MAX_LENGTH);
        assert_eq!(cut.chars().count(), MAX_LENGTH);
    }

    #[test]
    fn the_default_ignore_list_matches_common_scans() {
        let ignore = NotFoundLogSettings::default().ignore;
        for path in [
            "/wp-admin/install.php",
            "/wp-login.php",
            "/xmlrpc.php",
            "/.env",
            "/.git/config",
            "/cgi-bin/test",
        ] {
            assert!(is_ignored(path, &ignore), "{path}");
        }
        assert!(!is_ignored("/about-us", &ignore));
    }

    #[test]
    fn ignore_matching_is_case_insensitive() {
        let ignore = vec!["WP-ADMIN".to_string()];
        assert!(is_ignored("/Wp-Admin/x", &ignore));
    }

    #[test]
    fn settings_default_when_unset() {
        let settings = NotFoundLogSettings::from_value(None);
        assert_eq!(settings, NotFoundLogSettings::default());
    }

    #[test]
    fn settings_round_trip_through_a_value() {
        let settings = NotFoundLogSettings {
            enabled: false,
            retention_days: 30,
            max_rows: 500,
            ignore: vec!["foo".to_string()],
        };
        let round_tripped = NotFoundLogSettings::from_value(Some(&settings.to_value()));
        assert_eq!(settings, round_tripped);
    }

    #[test]
    fn should_record_only_a_404_get_or_head_when_enabled_and_not_skipped() {
        let on = NotFoundLogSettings::default();
        let off = NotFoundLogSettings {
            enabled: false,
            ..NotFoundLogSettings::default()
        };
        assert!(should_record(
            StatusCode::NOT_FOUND,
            &Method::GET,
            "/broken",
            &on
        ));
        assert!(should_record(
            StatusCode::NOT_FOUND,
            &Method::HEAD,
            "/broken",
            &on
        ));
        assert!(
            !should_record(StatusCode::OK, &Method::GET, "/broken", &on),
            "not a 404"
        );
        assert!(
            !should_record(StatusCode::NOT_FOUND, &Method::POST, "/broken", &on),
            "not a GET or HEAD"
        );
        assert!(
            !should_record(StatusCode::NOT_FOUND, &Method::GET, "/broken", &off),
            "the log is turned off"
        );
        assert!(
            !should_record(StatusCode::NOT_FOUND, &Method::GET, "/admin/x", &on),
            "a system path"
        );
        assert!(
            !should_record(StatusCode::NOT_FOUND, &Method::GET, "/x/wp-login.php", &on),
            "on the ignore list"
        );
    }

    #[test]
    fn a_non_positive_retention_or_max_rows_keeps_the_default() {
        let settings =
            NotFoundLogSettings::from_value(Some(&serde_json::json!({ "retention_days": 0, "max_rows": -5 })));
        assert_eq!(settings.retention_days, DEFAULT_RETENTION_DAYS);
        assert_eq!(settings.max_rows, DEFAULT_MAX_ROWS);
    }
}
