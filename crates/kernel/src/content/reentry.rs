//! What an item write or access decision is nested inside.
//!
//! Plugin item writes go through [`ItemService`](super::ItemService), so a
//! plugin's write fires the item write taps, and a tap handling one may write
//! again. Two task-locals bound that, both decided in
//! `docs/design/plugin-item-plumbing.md`:
//!
//! - **The write stack**: the plugins whose item write tap is running right now.
//!   A nested write fires its taps to every plugin *except* those on the stack,
//!   so a plugin never sees its own write echoed back into the handler that made
//!   it, and a write nested [`MAX_ITEM_WRITE_DEPTH`] deep is refused with
//!   [`WriteTooDeep`].
//! - **The access marker**: set while `tap_item_access` or `tap_field_access`
//!   is dispatched. A plugin handling either can call `get-item`, which asks for
//!   a decision, which dispatches the tap again, without end; an item-api call
//!   made while the marker is set is refused.
//!
//! Task-locals, not fields on the request state, because every dispatch builds a
//! **fresh** `RequestState` for its handler (`ItemService::tap_state`), so a
//! field would not reach the nested call. Tap dispatch awaits its handlers
//! inline on the calling task (nothing under `tap/` spawns), so a task-local
//! does, and being per task, two concurrent requests never see each other's.

use std::future::Future;

/// How many plugins' write taps may be running, one inside the other, before a
/// further write is refused.
///
/// Exclusion alone terminates (every level takes one more plugin out of what a
/// nested write can reach), so this bounds the worst case at a small number
/// rather than at the number of installed plugins.
pub const MAX_ITEM_WRITE_DEPTH: usize = 4;

tokio::task_local! {
    static WRITE_TAPS: Vec<String>;
    static DECIDING_ACCESS: ();
}

/// A write refused because it is nested [`MAX_ITEM_WRITE_DEPTH`] write taps
/// deep. Returned by `ItemService::create`, `update` and `delete` before
/// anything is written; `item-api` reports it as `ERR_ITEM_WRITE_DEPTH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteTooDeep;

impl std::fmt::Display for WriteTooDeep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "item write nested more than {MAX_ITEM_WRITE_DEPTH} write taps deep"
        )
    }
}

impl std::error::Error for WriteTooDeep {}

/// The plugins whose write tap is running around this call, outermost first.
fn stack() -> Vec<String> {
    WRITE_TAPS.try_with(Clone::clone).unwrap_or_default()
}

/// How many write taps this call is nested inside.
pub(crate) fn write_depth() -> usize {
    WRITE_TAPS.try_with(Vec::len).unwrap_or(0)
}

/// Whether `plugin`'s write tap is already running around this call.
pub(crate) fn is_handling_write(plugin: &str) -> bool {
    WRITE_TAPS
        .try_with(|s| s.iter().any(|p| p == plugin))
        .unwrap_or(false)
}

/// Run `plugin`'s write tap with the plugin pushed on the stack.
pub(crate) async fn handling_write<F: Future>(plugin: &str, f: F) -> F::Output {
    let mut next = stack();
    next.push(plugin.to_string());
    WRITE_TAPS.scope(next, f).await
}

/// Whether this call is inside an access decision.
pub(crate) fn inside_access_decision() -> bool {
    DECIDING_ACCESS.try_with(|()| ()).is_ok()
}

/// Dispatch an access decision with the marker set.
pub(crate) async fn deciding_access<F: Future>(f: F) -> F::Output {
    DECIDING_ACCESS.scope((), f).await
}

/// Run a future as though an access decision were in progress.
///
/// The marker is a task-local, so a test cannot set it from outside without a
/// hook. It exists so the guard can be tested for what it promises, that an
/// item-api call made inside a decision is refused, without a fixture plugin
/// whose `tap_item_access` handler calls back in.
#[doc(hidden)]
pub async fn deciding_access_for_test<F: Future>(f: F) -> F::Output {
    deciding_access(f).await
}

/// Run a future as though the named plugins' write taps were running around it,
/// outermost first. The hook for testing the depth bound without installing
/// [`MAX_ITEM_WRITE_DEPTH`] fixture plugins that each write from a tap.
#[doc(hidden)]
pub async fn handling_writes_for_test<F: Future>(plugins: Vec<String>, f: F) -> F::Output {
    WRITE_TAPS.scope(plugins, f).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_stack_grows_by_one_per_level_and_is_empty_outside() {
        assert_eq!(write_depth(), 0);
        assert!(!is_handling_write("a"));
        handling_write("a", async {
            assert_eq!(write_depth(), 1);
            assert!(is_handling_write("a"));
            handling_write("b", async {
                assert_eq!(write_depth(), 2);
                assert!(is_handling_write("a") && is_handling_write("b"));
            })
            .await;
            assert_eq!(write_depth(), 1, "the inner scope ends with its future");
        })
        .await;
        assert_eq!(write_depth(), 0);
    }

    #[tokio::test]
    async fn the_access_marker_is_set_only_inside_a_decision() {
        assert!(!inside_access_decision());
        deciding_access(async { assert!(inside_access_decision()) }).await;
        assert!(!inside_access_decision());
    }
}
