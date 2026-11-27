//! Cache tags: what a response was built from (B303).
//!
//! Every tag a page's data came from is collected into a task-local set while
//! the request runs, so [`crate::middleware::edge_cache`] can tell a CDN what
//! to drop when one of those things changes. Recording outside a request
//! that is being made public (cron, the queue worker, a command line
//! process) finds no collector in scope and is a no-op, the same shape
//! [`crate::telemetry::profile`] uses for its per-request counters.
//!
//! # The vocabulary
//!
//! - [`SITE`] — every response this layer makes public renders the menus,
//!   tiles, aliases, site settings, theme and language, so every one of them
//!   carries this tag. A write that is not about one item or one gather
//!   (`SiteConfig::set`, a theme change, a menu link) purges it.
//! - [`item_tag`] — `item:<uuid>`, one per item an `ItemService` load reads
//!   while serving the request.
//! - [`ITEMS`] — any response that ran a gather query. One tag, not one per
//!   row: a gather's result depends on every item that could have matched,
//!   which is unbounded, so purging it keyed on the query or its rows would
//!   either miss a write to a row outside the current page or purge a key per
//!   row for a result that may hold hundreds.
use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

/// Tag every public response carries: the write surfaces that answer every
/// page the same way (menus, tiles, aliases, site settings, theme, language).
pub const SITE: &str = "site";

/// Tag a response carries when it ran at least one gather query.
pub const ITEMS: &str = "items";

/// The tag for one item: `item:<uuid>`.
pub fn item_tag(id: Uuid) -> String {
    format!("item:{id}")
}

/// Whether `tag` uses only the vocabulary's allowed characters: letters,
/// digits, `:`, `_` and `-` (enough for `site`, `items` and `item:<uuid>`,
/// and for a tag a plugin or an administrator names by hand). Checked on the
/// admin "Purge the CDN" form, so a pasted tag cannot carry anything a CDN's
/// purge API would choke on.
pub fn is_valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-'))
}

/// At most this many tags on one manual purge from the admin.
pub const MAX_MANUAL_TAGS: usize = 500;

/// The tag header must stay under this many bytes (a CDN's own header-size
/// limit, Cloudflare's among them). Past it, [`header_value`] drops the
/// `item:` tags and keeps [`SITE`] and [`ITEMS`]: any write that would purge
/// an `item:` tag also purges `items` ([`crate::cache::purge`]), so nothing
/// a visitor sees goes stale, it just purges somewhat more than strictly
/// needed on a page built from an unusually large number of items.
pub const MAX_HEADER_BYTES: usize = 8 * 1024;

/// A request's tag collector: created by the caller, read by the caller
/// after [`scope`]'s future resolves.
///
/// Held as an `Arc` rather than read back through the task-local directly:
/// `tokio::task_local!`'s binding exists only while the scoped future is
/// being polled, so reading it from the task-local itself after `scope(...)
/// .await` returns would find nothing — exactly the shape
/// [`crate::telemetry::profile::RequestProfile`] uses, created outside its
/// own `scope` and read by the same caller once it returns.
#[derive(Default)]
pub struct TagCollector {
    tags: Mutex<HashSet<String>>,
}

impl TagCollector {
    /// A fresh, empty collector.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The tags recorded so far, sorted for a stable header.
    pub fn snapshot(&self) -> Vec<String> {
        let mut tags: Vec<String> = self
            .tags
            .lock()
            .map(|t| t.iter().cloned().collect())
            .unwrap_or_default();
        tags.sort();
        tags
    }
}

tokio::task_local! {
    static TAGS: Arc<TagCollector>;
}

/// Run `fut` with `collector` in scope.
pub async fn scope<F: Future>(collector: Arc<TagCollector>, fut: F) -> F::Output {
    TAGS.scope(collector, fut).await
}

/// Record that the response being built depended on `tag`. A no-op when no
/// collector is in scope.
pub fn record(tag: impl Into<String>) {
    let _ = TAGS.try_with(|collector| {
        if let Ok(mut tags) = collector.tags.lock() {
            tags.insert(tag.into());
        }
    });
}

/// Record that the response depended on item `id`.
pub fn record_item(id: Uuid) {
    record(item_tag(id));
}

/// Record that the response ran a gather query.
pub fn record_gather() {
    record(ITEMS);
}

/// Build the tag header value for a public response: every recorded tag plus
/// [`SITE`], comma-separated. When the joined value would exceed
/// [`MAX_HEADER_BYTES`], the `item:` tags are dropped and [`SITE`] plus
/// [`ITEMS`] stand in for them.
pub fn header_value(recorded: &[String]) -> String {
    let mut tags: Vec<&str> = std::iter::once(SITE)
        .chain(recorded.iter().map(String::as_str))
        .collect();
    tags.sort_unstable();
    tags.dedup();
    let joined = tags.join(",");
    if joined.len() <= MAX_HEADER_BYTES {
        return joined;
    }
    let mut fallback: Vec<&str> = vec![SITE, ITEMS];
    fallback.sort_unstable();
    fallback.dedup();
    fallback.join(",")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn item_tag_formats_the_uuid() {
        let id = Uuid::nil();
        assert_eq!(item_tag(id), format!("item:{id}"));
    }

    #[tokio::test]
    async fn record_outside_scope_is_a_no_op() {
        record("item:anything");
        let collector = TagCollector::new();
        scope(collector.clone(), async {}).await;
        assert!(collector.snapshot().is_empty());
    }

    #[tokio::test]
    async fn recorded_tags_are_read_after_the_scope_ends() {
        let collector = TagCollector::new();
        scope(collector.clone(), async {
            record_item(Uuid::nil());
            record_gather();
        })
        .await;
        // The point of the test: read the collector *after* `scope` has
        // returned, the same way `edge_cache_headers` reads it after
        // `next.run` — not from inside the scoped future.
        let snap = collector.snapshot();
        assert!(snap.contains(&item_tag(Uuid::nil())));
        assert!(snap.contains(&ITEMS.to_string()));
    }

    #[tokio::test]
    async fn each_scope_starts_empty() {
        let first = TagCollector::new();
        scope(first.clone(), async {
            record("item:leftover");
        })
        .await;
        let second = TagCollector::new();
        scope(second.clone(), async {}).await;
        assert!(second.snapshot().is_empty());
    }

    #[test]
    fn header_value_always_carries_site() {
        assert_eq!(header_value(&[]), SITE);
    }

    #[test]
    fn header_value_includes_recorded_tags_sorted() {
        let recorded = vec!["item:b".to_string(), "item:a".to_string()];
        assert_eq!(header_value(&recorded), "item:a,item:b,site");
    }

    #[test]
    fn is_valid_tag_accepts_the_vocabulary() {
        assert!(is_valid_tag("site"));
        assert!(is_valid_tag("items"));
        assert!(is_valid_tag(&item_tag(Uuid::nil())));
    }

    #[test]
    fn is_valid_tag_rejects_anything_else() {
        assert!(!is_valid_tag(""));
        assert!(!is_valid_tag("tag with spaces"));
        assert!(!is_valid_tag("tag,with,commas"));
        assert!(!is_valid_tag("<script>"));
    }

    #[test]
    fn header_value_falls_back_when_too_long() {
        let recorded: Vec<String> = (0..2000).map(|i| format!("item:{i:032}")).collect();
        let value = header_value(&recorded);
        assert!(value.len() <= MAX_HEADER_BYTES);
        assert_eq!(value, "items,site");
    }
}
