//! Feed scheduling: pick which feeds are due and advance a round-robin cursor
//! (M1-9).
//!
//! `tap_cron` fires every cron cycle with only a timestamp (no per-key
//! dispatch), so the plugin decides *which* feeds to enqueue each tick. A feed
//! is due when `now - last_fetched_at >= interval` (or it has never been
//! fetched). To bound work per tick and give every feed a fair turn under a
//! backlog, due feeds are taken in a round-robin order persisted as a cursor —
//! the `ritrovo_importer` pattern, but selecting only *due* feeds.

use crate::ports::FeedSchedule;

/// The outcome of a due-feed selection pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueSelection {
    /// Feed ids to enqueue for fetch this tick, in round-robin order.
    pub due: Vec<String>,
    /// The cursor to persist for the next tick.
    pub next_cursor: u64,
}

/// Whether a feed is due at `now`.
#[must_use]
pub fn is_due(feed: &FeedSchedule, now: i64) -> bool {
    match feed.last_fetched_at {
        None => true,
        Some(last) => now.saturating_sub(last) >= feed.interval_seconds,
    }
}

/// Select up to `limit` due feeds, round-robin, and compute the next cursor.
///
/// `feeds` is the full enabled set in a stable order (the store returns them
/// ordered by id). Selection starts at `cursor % feeds.len()` and walks the
/// ring, collecting due feeds until it has `limit` of them or has visited every
/// feed once. The next cursor advances past the last *visited* position so the
/// following tick starts where this one stopped scanning — this is what gives a
/// large, partly-due feed set fair rotation instead of always re-checking the
/// same prefix.
///
/// A `limit` of 0 selects nothing and leaves the cursor unchanged.
#[must_use]
pub fn select_due(feeds: &[FeedSchedule], now: i64, cursor: u64, limit: usize) -> DueSelection {
    let n = feeds.len();
    if n == 0 || limit == 0 {
        return DueSelection {
            due: Vec::new(),
            next_cursor: cursor,
        };
    }
    let start = (cursor % n as u64) as usize;
    let mut due = Vec::new();
    let mut visited = 0usize;
    let mut pos = start;
    while visited < n && due.len() < limit {
        let feed = &feeds[pos];
        if is_due(feed, now) {
            due.push(feed.id.clone());
        }
        pos = (pos + 1) % n;
        visited += 1;
    }
    // Advance the cursor past everything we scanned this tick.
    let next_cursor = cursor.wrapping_add(visited as u64);
    DueSelection { due, next_cursor }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn feed(id: &str, interval: i64, last: Option<i64>) -> FeedSchedule {
        FeedSchedule {
            id: id.to_string(),
            interval_seconds: interval,
            last_fetched_at: last,
        }
    }

    #[test]
    fn never_fetched_is_due() {
        assert!(is_due(&feed("a", 900, None), 1000));
    }

    #[test]
    fn due_after_interval() {
        assert!(!is_due(&feed("a", 900, Some(1000)), 1500)); // 500 < 900
        assert!(is_due(&feed("a", 900, Some(1000)), 1900)); // 900 >= 900
    }

    #[test]
    fn selects_only_due() {
        let feeds = vec![
            feed("a", 900, None),         // due
            feed("b", 900, Some(10_000)), // not due at now=10_100
            feed("c", 900, Some(0)),      // due
        ];
        let sel = select_due(&feeds, 10_100, 0, 10);
        assert_eq!(sel.due, vec!["a".to_string(), "c".to_string()]);
        assert_eq!(sel.next_cursor, 3); // visited all three
    }

    #[test]
    fn respects_limit() {
        let feeds = vec![feed("a", 1, None), feed("b", 1, None), feed("c", 1, None)];
        let sel = select_due(&feeds, 100, 0, 2);
        assert_eq!(sel.due.len(), 2);
        assert_eq!(sel.due, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(sel.next_cursor, 2);
    }

    #[test]
    fn cursor_rotates_start_point() {
        let feeds = vec![feed("a", 1, None), feed("b", 1, None), feed("c", 1, None)];
        // Start at cursor=2 → begins at index 2 (c), wraps to a, b.
        let sel = select_due(&feeds, 100, 2, 2);
        assert_eq!(sel.due, vec!["c".to_string(), "a".to_string()]);
    }

    #[test]
    fn empty_and_zero_limit_noops() {
        assert_eq!(select_due(&[], 100, 5, 10).due.len(), 0);
        assert_eq!(select_due(&[], 100, 5, 10).next_cursor, 5);
        let feeds = vec![feed("a", 1, None)];
        let sel = select_due(&feeds, 100, 5, 0);
        assert!(sel.due.is_empty());
        assert_eq!(sel.next_cursor, 5);
    }
}
