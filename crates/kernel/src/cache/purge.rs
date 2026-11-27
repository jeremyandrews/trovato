//! The pending CDN purge, drained once a second (B303).
//!
//! A write that changes what an anonymous visitor sees calls [`purge`] (or
//! [`purge_all`]) right after it commits, adding tags to a process-wide
//! pending set. A background task, started with the server, drains the set
//! on [`DRAIN_INTERVAL`] and dispatches `tap_cache_purge` once with every
//! pending tag — see [`crate::cron::CronService::drain_cache_purge_now`],
//! which does the dispatch and is what the background task and the tests both
//! call.
//!
//! Nothing here knows about plugins or taps: a process whose server loop
//! never runs (a one-shot CLI command such as `config import`) records into
//! its own pending set same as the server does, but exits before anything
//! ever drains it, so nothing is dispatched — which is exactly the contract
//! command-line writes have (they wait out `s-maxage`, same as before B303).

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

/// How often the pending set is drained.
pub const DRAIN_INTERVAL: Duration = Duration::from_secs(1);

/// More pending tags than this collapses the purge into "everything": fewer
/// round trips to a CDN API that charges per call, and no plugin need accept
/// an unbounded request body.
pub const MAX_PENDING_TAGS: usize = 500;

#[derive(Default)]
struct Pending {
    tags: HashSet<String>,
    all: bool,
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

fn with_pending<R>(f: impl FnOnce(&mut Pending) -> R) -> R {
    let mut guard = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Pending::default))
}

/// Record that every one of `tags` is stale on the CDN.
pub fn purge(tags: &[&str]) {
    with_pending(|pending| {
        for tag in tags {
            pending.tags.insert((*tag).to_string());
        }
    });
}

/// Record that everything is stale on the CDN (a stage publish, or an
/// administrator's "purge everything").
pub fn purge_all() {
    with_pending(|pending| pending.all = true);
}

/// The purge payload `tap_cache_purge` is dispatched with, draining the
/// pending set. `None` when there is nothing pending, so the caller dispatches
/// nothing (not even an empty notification).
///
/// `{"tags": [...], "all": false}`, or `{"tags": [], "all": true}` when
/// [`purge_all`] was called or more than [`MAX_PENDING_TAGS`] are pending.
pub(crate) fn take_pending_json() -> Option<String> {
    let pending = with_pending(std::mem::take);
    if !pending.all && pending.tags.is_empty() {
        return None;
    }
    let all = pending.all || pending.tags.len() > MAX_PENDING_TAGS;
    let mut tags: Vec<String> = if all {
        Vec::new()
    } else {
        pending.tags.into_iter().collect()
    };
    tags.sort();
    Some(
        serde_json::json!({ "tags": tags, "all": all })
            .to_string(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn reset() {
        with_pending(|p| {
            p.tags.clear();
            p.all = false;
        });
    }

    /// `PENDING` is one process-wide static, so every case here runs in a
    /// single `#[test]`, in order, rather than risk two cases racing each
    /// other under `cargo test`'s default parallel unit-test threads.
    #[test]
    fn pending_set_behaviour() {
        reset();
        assert_eq!(take_pending_json(), None, "nothing pending drains to none");

        purge(&["item:1", "items"]);
        let json: serde_json::Value =
            serde_json::from_str(&take_pending_json().unwrap()).unwrap();
        assert_eq!(json["all"], false);
        let mut tags: Vec<String> = json["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        tags.sort();
        assert_eq!(tags, vec!["item:1".to_string(), "items".to_string()]);
        // Draining clears the set.
        assert_eq!(take_pending_json(), None);

        purge(&["item:1"]);
        purge_all();
        let json: serde_json::Value =
            serde_json::from_str(&take_pending_json().unwrap()).unwrap();
        assert_eq!(json["all"], true, "purge_all wins over pending tags");
        assert_eq!(json["tags"].as_array().unwrap().len(), 0);

        let many: Vec<String> = (0..MAX_PENDING_TAGS + 1)
            .map(|i| format!("item:{i}"))
            .collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        purge(&refs);
        let json: serde_json::Value =
            serde_json::from_str(&take_pending_json().unwrap()).unwrap();
        assert_eq!(
            json["all"], true,
            "more than MAX_PENDING_TAGS collapses to all"
        );
        assert_eq!(json["tags"].as_array().unwrap().len(), 0);

        reset();
    }
}
