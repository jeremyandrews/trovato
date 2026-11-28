//! Edge caching: a Trovato site behind a CDN (B303).
//!
//! An anonymous HTML response can carry `Cache-Control: public` and a list of
//! the tags it was built from, so a CDN caches it and knows what to drop when
//! one of those tags changes. The `edge_cache` [`middleware`](crate::middleware)
//! decides the headers; this module is the other half: the pending set a
//! write adds tags to after it commits ([`purge`], [`purge_everything`]), and
//! the background drain that turns pending tags into one `tap_cache_purge`
//! dispatch to whichever purge plugin is enabled.
//!
//! See `docs/deployment/cdn.md` for the full picture, including the
//! write-to-tags table and the two shipped purge plugins.

pub mod tags;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::PgPool;

use crate::state::AppState;
use crate::tap::{RequestServices, RequestState, TapDispatcher, UserContext};

/// How often the pending set is drained and dispatched.
pub const DRAIN_INTERVAL: Duration = Duration::from_secs(1);

/// Above this many distinct pending tags, a drain asks for everything instead
/// of naming each one.
pub const MAX_PENDING_TAGS: usize = 500;

/// The tap plugins implement to receive a purge.
const TAP_CACHE_PURGE: &str = "tap_cache_purge";

#[derive(Default)]
struct Pending {
    tags: BTreeSet<String>,
    all: bool,
}

static PENDING: Mutex<Pending> = Mutex::new(Pending {
    tags: BTreeSet::new(),
    all: false,
});

/// Record that `tags` changed: call it right after the write that changed
/// them commits.
pub fn purge<I, S>(tags: I)
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if pending.all {
        return;
    }
    pending.tags.extend(tags.into_iter().map(Into::into));
    if pending.tags.len() > MAX_PENDING_TAGS {
        pending.all = true;
        pending.tags.clear();
    }
}

/// Record that everything changed: a stage publish, or an explicit "purge
/// everything" from the admin.
pub fn purge_everything() {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    pending.all = true;
    pending.tags.clear();
}

/// Take whatever is pending, leaving the set empty.
fn take_pending() -> Pending {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    std::mem::take(&mut *pending)
}

/// What one drain did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainOutcome {
    /// Nothing was pending.
    Nothing,
    /// Tags were pending, but no enabled plugin implements `tap_cache_purge`:
    /// dropped.
    NoPlugin,
    /// Dispatched. `tags` is empty when `all` is true.
    Dispatched { tags: Vec<String>, all: bool },
}

/// One drain: take whatever is pending and, if an enabled plugin implements
/// `tap_cache_purge`, dispatch it once. When nothing is pending, or no plugin
/// implements the tap, this costs a lock and a clear and nothing else.
///
/// Takes the dispatcher and pool directly, rather than an [`AppState`], so a
/// test can dispatch to a plugin it loaded into a dispatcher of its own
/// without needing a whole app built around that plugin — see
/// [`drain_now_with`].
async fn drain(dispatcher: &Arc<TapDispatcher>, db: &PgPool) -> DrainOutcome {
    let pending = take_pending();
    if pending.tags.is_empty() && !pending.all {
        return DrainOutcome::Nothing;
    }

    if dispatcher.registry().handler_count(TAP_CACHE_PURGE) == 0 {
        return DrainOutcome::NoPlugin;
    }

    let tags: Vec<String> = if pending.all {
        Vec::new()
    } else {
        pending.tags.into_iter().collect()
    };
    let payload = serde_json::json!({ "tags": tags, "all": pending.all });
    let Ok(payload_json) = serde_json::to_string(&payload) else {
        return DrainOutcome::NoPlugin;
    };

    let services = RequestServices::for_background(db.clone(), None, None, reqwest::Client::new())
        .with_plugin_runtime(dispatcher.runtime().clone());
    let request_state = RequestState::new(UserContext::background(), services);

    // A notification: a plugin error is already logged by the dispatcher and
    // does not reach the writer that recorded the purge.
    dispatcher
        .dispatch(TAP_CACHE_PURGE, &payload_json, request_state)
        .await;

    DrainOutcome::Dispatched {
        tags,
        all: pending.all,
    }
}

/// Start the background drain. Call once, when the server starts; it runs
/// until the process exits.
pub fn spawn_drain(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(DRAIN_INTERVAL);
        loop {
            tick.tick().await;
            drain(state.tap_dispatcher(), state.db()).await;
        }
    });
}

/// Drain at once, ignoring the interval: for the admin's manual purge, and
/// for a test that must see the dispatch before it asserts.
#[doc(hidden)]
pub async fn drain_now(state: &AppState) -> DrainOutcome {
    drain(state.tap_dispatcher(), state.db()).await
}

/// As [`drain_now`], against an explicit dispatcher and pool rather than a
/// whole [`AppState`]: for a test that loaded a purge plugin into its own
/// [`TapDispatcher`] (see `plugin_queue_v2_test.rs` for the pattern) instead
/// of building a full app around it.
#[doc(hidden)]
pub async fn drain_now_with(dispatcher: &Arc<TapDispatcher>, db: &PgPool) -> DrainOutcome {
    drain(dispatcher, db).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// `PENDING` is one process-wide static; serialize every test that
    /// touches it so they cannot interleave.
    static TEST_SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn purge_accumulates_tags() {
        let _guard = TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        // Each test's assertions are against the one process-wide pending
        // set, so start from a state this test controls.
        purge_everything();
        let _ = take_pending();

        purge(["a".to_string(), "b".to_string()]);
        purge(["b".to_string(), "c".to_string()]);
        let pending = take_pending();
        assert!(!pending.all);
        assert_eq!(
            pending.tags,
            BTreeSet::from(["a".to_string(), "b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn purge_everything_wins_over_pending_tags() {
        let _guard = TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        purge_everything();
        let _ = take_pending();

        purge(["a".to_string()]);
        purge_everything();
        purge(["b".to_string()]);
        let pending = take_pending();
        assert!(pending.all);
        assert!(pending.tags.is_empty(), "everything subsumes named tags");
    }

    #[test]
    fn too_many_distinct_tags_becomes_everything() {
        let _guard = TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        purge_everything();
        let _ = take_pending();

        purge((0..=MAX_PENDING_TAGS).map(|n| format!("item:{n}")));
        let pending = take_pending();
        assert!(pending.all, "over the cap must fall back to everything");
        assert!(pending.tags.is_empty());
    }

    #[test]
    fn taking_pending_clears_it() {
        let _guard = TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        purge_everything();
        let _ = take_pending();
        assert_eq!(take_pending(), Pending::default());
    }

    impl PartialEq for Pending {
        fn eq(&self, other: &Self) -> bool {
            self.tags == other.tags && self.all == other.all
        }
    }
    impl Eq for Pending {}
    impl std::fmt::Debug for Pending {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Pending")
                .field("tags", &self.tags)
                .field("all", &self.all)
                .finish()
        }
    }
}
