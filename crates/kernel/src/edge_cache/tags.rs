//! The cache tag vocabulary, and the per-request collector that records what
//! a response was built from.
//!
//! Three kinds of tag: [`SITE`] (added by the `edge_cache` middleware to
//! every response it marks public — menus, tiles, aliases, settings, theme
//! and language all render on every page, so nothing else needs to ask for
//! it), [`ITEMS`] (a response that ran a gather query), and [`item_tag`] (one
//! per item loaded while serving a response). [`record_item`] and
//! [`record_items`] add to whichever collector [`scope`] opened for the
//! current task; outside one (cron, the queue worker, a command line
//! process) they are a no-op.

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

/// Every response the `edge_cache` middleware marks public carries this: it
/// rendered menus, tiles, aliases, site settings, the theme and the language,
/// all of which are present on every page.
pub const SITE: &str = "site";

/// A response that ran a gather query carries this.
pub const ITEMS: &str = "items";

/// The tag for one item.
pub fn item_tag(id: Uuid) -> String {
    format!("item:{id}")
}

tokio::task_local! {
    static COLLECTOR: Arc<Mutex<BTreeSet<String>>>;
}

/// Run `fut` with `tags` as the collector in scope. The `edge_cache`
/// middleware reads `tags` after `fut` resolves.
pub async fn scope<F: Future>(tags: Arc<Mutex<BTreeSet<String>>>, fut: F) -> F::Output {
    COLLECTOR.scope(tags, fut).await
}

fn record(tag: impl Into<String>) {
    let _ = COLLECTOR.try_with(|collector| {
        if let Ok(mut set) = collector.lock() {
            set.insert(tag.into());
        }
    });
}

/// Record that this response loaded item `id`.
pub fn record_item(id: Uuid) {
    record(item_tag(id));
}

/// Record that this response ran a gather query.
pub fn record_items() {
    record(ITEMS);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn item_tag_format() {
        let id = Uuid::nil();
        assert_eq!(item_tag(id), format!("item:{id}"));
    }

    #[tokio::test]
    async fn recording_outside_a_scope_is_a_no_op() {
        // No active collector: nothing panics, nothing is recorded anywhere
        // reachable.
        record_item(Uuid::nil());
        record_items();
    }

    #[tokio::test]
    async fn recording_inside_a_scope_lands_in_the_collector() {
        let collector = Arc::new(Mutex::new(BTreeSet::new()));
        let id = Uuid::now_v7();
        scope(collector.clone(), async {
            record_item(id);
            record_items();
            record_items();
        })
        .await;
        let collected = collector.lock().unwrap().clone();
        assert_eq!(collected, BTreeSet::from([item_tag(id), ITEMS.to_string()]));
    }

    #[tokio::test]
    async fn a_nested_scope_does_not_leak_into_the_outer_one() {
        let outer = Arc::new(Mutex::new(BTreeSet::new()));
        scope(outer.clone(), async {
            let inner = Arc::new(Mutex::new(BTreeSet::new()));
            scope(inner.clone(), async {
                record_items();
            })
            .await;
            assert_eq!(inner.lock().unwrap().len(), 1);
            // Back in the outer scope, nothing recorded.
            assert!(outer.lock().unwrap().is_empty());
        })
        .await;
    }
}
