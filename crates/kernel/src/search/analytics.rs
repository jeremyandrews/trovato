//! Search analytics: what visitors search for, counted per UTC day.
//!
//! Recording never touches the database on the request path. [`SearchAnalytics`]
//! keeps the counts in memory behind a short synchronous lock; a background task
//! (spawned in `main.rs`) calls [`SearchAnalytics::flush`] every
//! [`FLUSH_INTERVAL`] and once more at shutdown, writing one statement per table
//! that adds to whatever is there, so several kernel processes sum correctly.
//!
//! What is stored, and only this: the UTC day; the query after
//! [`normalize_for_analytics`] (lowercased, whitespace collapsed, email
//! addresses and runs of six or more digits replaced, at most 100 characters);
//! how many searches it had and how many found nothing; and per day the
//! expansion cache hits and misses and the follow-up sentiment counts. Never an
//! IP address, account, session, user agent, referrer or a time finer than the
//! day, in a table or in a log line.
//!
//! Bounded: at most [`MAX_KEYS_PER_WINDOW`] distinct (day, query) keys wait
//! between flushes; past that, new queries are dropped and the number dropped is
//! logged at the next flush. A flush that fails is logged and its batch
//! dropped, so memory never grows with an unreachable database. A crash loses at
//! most one flush window.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use sqlx::PgPool;

use super::{SearchSettings, normalize_query};

/// How often the background task flushes.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(30);

/// Most distinct (day, query) keys held between two flushes.
pub const MAX_KEYS_PER_WINDOW: usize = 10_000;

/// Longest stored query, in characters.
pub const MAX_QUERY_CHARS: usize = 100;

/// Digits in a row from which a number is treated as an identifier (a phone,
/// account or order number) and not stored.
pub const LONG_NUMBER_DIGITS: usize = 6;

/// The form a query is stored in, or `None` when nothing is left to store.
///
/// [`normalize_query`] first (trim, collapse whitespace, lowercase), then every
/// email shaped token becomes `[email]` and every run of six or more digits
/// `[number]`, then the result is cut to [`MAX_QUERY_CHARS`] characters.
pub fn normalize_for_analytics(query: &str) -> Option<String> {
    let normalized = normalize_query(query);
    let redacted = normalized
        .split(' ')
        .map(|token| {
            if is_email_shaped(token) {
                "[email]".to_string()
            } else {
                replace_long_numbers(token)
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let stored: String = redacted.chars().take(MAX_QUERY_CHARS).collect();
    let stored = stored.trim_end().to_string();
    (!stored.is_empty()).then_some(stored)
}

/// Whether `token` looks like an email address: something, `@`, then a domain
/// with a dot that has something on both sides.
fn is_email_shaped(token: &str) -> bool {
    let Some((local, domain)) = token.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain
            .rsplit_once('.')
            .is_some_and(|(host, tld)| !host.is_empty() && !tld.is_empty())
}

/// `token` with every run of [`LONG_NUMBER_DIGITS`] or more digits replaced by
/// `[number]`.
fn replace_long_numbers(token: &str) -> String {
    fn end_run(run: &mut String, out: &mut String) {
        if run.chars().count() >= LONG_NUMBER_DIGITS {
            out.push_str("[number]");
        } else {
            out.push_str(run);
        }
        run.clear();
    }
    let mut out = String::with_capacity(token.len());
    let mut run = String::new();
    for c in token.chars() {
        if c.is_numeric() {
            run.push(c);
        } else {
            end_run(&mut run, &mut out);
            out.push(c);
        }
    }
    end_run(&mut run, &mut out);
    out
}

/// A per day counter other than the query counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Counter {
    /// An expansion served from the cache.
    ExpansionHit,
    /// An expansion that had to go to the provider.
    ExpansionMiss,
    /// A follow-up classified as satisfied.
    Satisfied,
    /// A follow-up classified as confused.
    Confused,
    /// A follow-up classified as unmet.
    Unmet,
}

/// One day's counters, in [`Counter`] order.
type DayCounters = [i32; 5];

#[derive(Default)]
struct Pending {
    queries: HashMap<(NaiveDate, String), (i32, i32)>,
    days: HashMap<NaiveDate, DayCounters>,
    dropped: u64,
}

/// The in process aggregator. One per kernel process, owned by the search
/// service.
pub struct SearchAnalytics {
    pool: PgPool,
    pending: Mutex<Pending>,
    /// Held for the whole of a flush, so two flushes never interleave.
    flush_lock: tokio::sync::Mutex<()>,
}

impl SearchAnalytics {
    /// A new, empty aggregator writing to `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            pending: Mutex::new(Pending::default()),
            flush_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Count one search for `query` that found `result_count` results.
    ///
    /// Synchronous and non blocking: a short lock on the in memory map and
    /// nothing else. Does nothing when analytics is off or the query normalizes
    /// to nothing.
    pub fn record(&self, settings: &SearchSettings, query: &str, result_count: u64) {
        if !settings.analytics_enabled {
            return;
        }
        let Some(query) = normalize_for_analytics(query) else {
            return;
        };
        let key = (today(), query);
        let mut pending = self.lock_pending();
        let full = pending.queries.len() >= MAX_KEYS_PER_WINDOW;
        match pending.queries.get_mut(&key) {
            Some(entry) => add(entry, result_count),
            None if full => pending.dropped += 1,
            None => {
                let mut entry = (0, 0);
                add(&mut entry, result_count);
                pending.queries.insert(key, entry);
            }
        }
    }

    /// Add one to a per day counter. Synchronous and non blocking, like
    /// [`record`](Self::record), and likewise a no-op with analytics off.
    pub fn count(&self, settings: &SearchSettings, counter: Counter) {
        if !settings.analytics_enabled {
            return;
        }
        let mut pending = self.lock_pending();
        let day = pending.days.entry(today()).or_default();
        let slot = &mut day[counter as usize];
        *slot = slot.saturating_add(1);
    }

    /// Write everything pending, one statement per table, adding to the rows
    /// already there. A batch that fails to write is logged and dropped.
    pub async fn flush(&self) -> Result<()> {
        let _flushing = self.flush_lock.lock().await;
        let batch = std::mem::take(&mut *self.lock_pending());

        if batch.dropped > 0 {
            tracing::warn!(
                dropped = batch.dropped,
                limit = MAX_KEYS_PER_WINDOW,
                "search analytics dropped new queries past the per window limit"
            );
        }

        let result = write_batch(&self.pool, batch).await;
        if let Err(ref e) = result {
            tracing::warn!(error = %e, "search analytics flush failed; batch dropped");
        }
        result
    }

    /// Wait for the flush lock and hold it.
    ///
    /// For a caller that must keep the writer still: a test proving that
    /// recording does not wait on it.
    #[doc(hidden)]
    pub async fn lock_flush(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.flush_lock.lock().await
    }

    /// Delete rows older than `retention_days` from both tables, one `DELETE`
    /// each. Returns the number of rows deleted.
    pub async fn prune(&self, retention_days: u32) -> Result<u64> {
        let cutoff = today() - chrono::Duration::days(i64::from(retention_days));
        let queries = sqlx::query("DELETE FROM search_query_daily WHERE day < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .context("prune search_query_daily")?
            .rows_affected();
        let days = sqlx::query("DELETE FROM search_daily WHERE day < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .context("prune search_daily")?
            .rows_affected();
        Ok(queries + days)
    }

    fn lock_pending(&self) -> std::sync::MutexGuard<'_, Pending> {
        // A panic while holding this lock leaves plain counters behind, which
        // are still safe to use.
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn add(entry: &mut (i32, i32), result_count: u64) {
    entry.0 = entry.0.saturating_add(1);
    if result_count == 0 {
        entry.1 = entry.1.saturating_add(1);
    }
}

/// Today in UTC: the finest time analytics keeps.
fn today() -> NaiveDate {
    chrono::Utc::now().date_naive()
}

async fn write_batch(pool: &PgPool, batch: Pending) -> Result<()> {
    if !batch.queries.is_empty() {
        let mut days = Vec::with_capacity(batch.queries.len());
        let mut queries = Vec::with_capacity(batch.queries.len());
        let mut searches = Vec::with_capacity(batch.queries.len());
        let mut zero = Vec::with_capacity(batch.queries.len());
        for ((day, query), (s, z)) in batch.queries {
            days.push(day);
            queries.push(query);
            searches.push(s);
            zero.push(z);
        }
        sqlx::query(
            r#"
            INSERT INTO search_query_daily (day, query, searches, zero_results)
            SELECT * FROM unnest($1::date[], $2::text[], $3::int[], $4::int[])
            ON CONFLICT (day, query) DO UPDATE SET
                searches = search_query_daily.searches + EXCLUDED.searches,
                zero_results = search_query_daily.zero_results + EXCLUDED.zero_results
            "#,
        )
        .bind(&days)
        .bind(&queries)
        .bind(&searches)
        .bind(&zero)
        .execute(pool)
        .await
        .context("flush search_query_daily")?;
    }

    if !batch.days.is_empty() {
        let mut days = Vec::with_capacity(batch.days.len());
        let mut columns: [Vec<i32>; 5] = Default::default();
        for (day, counters) in batch.days {
            days.push(day);
            for (column, value) in columns.iter_mut().zip(counters) {
                column.push(value);
            }
        }
        let [hits, misses, satisfied, confused, unmet] = columns;
        sqlx::query(
            r#"
            INSERT INTO search_daily
                (day, expansion_hits, expansion_misses, satisfied, confused, unmet)
            SELECT * FROM unnest($1::date[], $2::int[], $3::int[], $4::int[], $5::int[], $6::int[])
            ON CONFLICT (day) DO UPDATE SET
                expansion_hits = search_daily.expansion_hits + EXCLUDED.expansion_hits,
                expansion_misses = search_daily.expansion_misses + EXCLUDED.expansion_misses,
                satisfied = search_daily.satisfied + EXCLUDED.satisfied,
                confused = search_daily.confused + EXCLUDED.confused,
                unmet = search_daily.unmet + EXCLUDED.unmet
            "#,
        )
        .bind(&days)
        .bind(&hits)
        .bind(&misses)
        .bind(&satisfied)
        .bind(&confused)
        .bind(&unmet)
        .execute(pool)
        .await
        .context("flush search_daily")?;
    }

    Ok(())
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn aggregator() -> SearchAnalytics {
        SearchAnalytics::new(
            PgPool::connect_lazy("postgres://localhost/unused").expect("a lazy pool"),
        )
    }

    #[test]
    fn emails_and_long_numbers_are_replaced() {
        assert_eq!(
            normalize_for_analytics("Mail a.b@c.io or (x@y.org) 1234567890 x12345678y 12345"),
            Some("mail [email] or [email] [number] x[number]y 12345".to_string())
        );
        assert_eq!(
            normalize_for_analytics("@handle and a@b"),
            Some("@handle and a@b".to_string())
        );
    }

    #[test]
    fn the_stored_query_is_at_most_100_characters() {
        let stored = normalize_for_analytics(&"ab ".repeat(80)).unwrap();
        assert!(stored.chars().count() <= MAX_QUERY_CHARS);
        assert!(!stored.ends_with(' '));
    }

    #[tokio::test]
    async fn the_window_is_bounded_and_counts_what_it_drops() {
        let a = aggregator();
        let on = SearchSettings::default();
        for i in 0..MAX_KEYS_PER_WINDOW {
            a.record(&on, &format!("q{i}"), 1);
        }
        // A known key still counts; a new one is dropped.
        a.record(&on, "q0", 0);
        a.record(&on, "brand new", 1);
        let pending = a.lock_pending();
        assert_eq!(pending.queries.len(), MAX_KEYS_PER_WINDOW);
        assert_eq!(pending.dropped, 1);
        assert_eq!(pending.queries[&(today(), "q0".to_string())], (2, 1));
    }

    #[tokio::test]
    async fn counters_add_up_per_day() {
        let a = aggregator();
        let on = SearchSettings::default();
        a.count(&on, Counter::ExpansionHit);
        a.count(&on, Counter::ExpansionHit);
        a.count(&on, Counter::Unmet);
        let off = SearchSettings {
            analytics_enabled: false,
            ..SearchSettings::default()
        };
        a.count(&off, Counter::Unmet);
        assert_eq!(a.lock_pending().days[&today()], [2, 0, 0, 0, 1]);
    }
}
