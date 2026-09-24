//! Host-call tracing: what the guest asked the host to do, and how long it took.
//!
//! Every host function a plugin can import is wrapped so that entering and
//! leaving it is observable. Two things come out of that:
//!
//! 1. **A paired log record.** `debug` on entry, `debug` on exit with the
//!    elapsed milliseconds, and a `warn` on exit when a call ran longer than
//!    [`SLOW_HOST_CALL_MS`]. A host call that is entered and never left leaves
//!    only its entry line, which is what names a wedged call in a log tail.
//! 2. **A live registry of in-flight calls** ([`in_flight`]). The guest inside
//!    one tap invocation is single-threaded, so an invocation has at most one
//!    host call outstanding at a time. Keying the registry by invocation id
//!    therefore answers, for any invocation, "which host call is it sitting in
//!    right now" — the question the queue drain has to answer when it gives up
//!    waiting on a job.
//!
//! The registry is the reason this is not merely debug logging: an abandoned
//! job has to be able to name the host call it was abandoned in, and by then
//! the call has produced no exit record to read.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use dashmap::DashMap;
use tracing::{debug, warn};
use wasmtime::{Caller, Linker};

use crate::plugin::PluginState;

/// A host call slower than this is reported at `warn` rather than `debug`.
///
/// The slowest legitimate host call is an AI provider request: analyze against
/// a large model was measured at 12–17 seconds. The threshold has to sit above
/// that (so ordinary traffic stays quiet) and far below the background-tap
/// epoch budget of [`crate::plugin::limits::BACKGROUND_TAP_EPOCH_DEADLINE_SECS`]
/// seconds (so a call heading for a wedge is visible long before any deadline
/// could fire). 30 seconds is about twice the observed worst case and a fifth
/// of the budget, which satisfies both.
pub const SLOW_HOST_CALL_MS: u128 = 30_000;

/// Source of [`PluginState::invocation_id`] values.
static NEXT_INVOCATION_ID: AtomicU64 = AtomicU64::new(1);

/// Allocate a process-unique invocation id for one plugin call.
pub(crate) fn next_invocation_id() -> u64 {
    NEXT_INVOCATION_ID.fetch_add(1, Ordering::Relaxed)
}

/// A host call that has been entered and has not yet returned.
#[derive(Debug, Clone)]
pub struct InFlightHostCall {
    /// Host interface module, e.g. `trovato:kernel/ai-api`.
    pub module: &'static str,
    /// Host function name within the module, e.g. `ai-request`.
    pub function: &'static str,
    /// The plugin whose guest code made the call.
    pub plugin: String,
    /// When the call was entered.
    pub started: Instant,
}

impl InFlightHostCall {
    /// `module::function`, the form used in log lines.
    pub fn qualified_name(&self) -> String {
        format!("{}::{}", self.module, self.function)
    }
}

/// In-flight host calls, keyed by invocation id.
///
/// At most one entry per invocation: the guest cannot call two host functions
/// at once within a single call.
static IN_FLIGHT: LazyLock<DashMap<u64, InFlightHostCall>> = LazyLock::new(DashMap::new);

/// The host call invocation `invocation_id` is currently sitting in, if any.
///
/// Read by the queue drain when it abandons a job, so the resulting error line
/// can name where the job was stuck rather than merely that it was.
pub fn in_flight(invocation_id: u64) -> Option<InFlightHostCall> {
    IN_FLIGHT.get(&invocation_id).map(|e| e.value().clone())
}

/// Record that `invocation_id` has entered a host call.
fn enter(
    invocation_id: u64,
    module: &'static str,
    function: &'static str,
    plugin: &str,
) -> Instant {
    let started = Instant::now();
    IN_FLIGHT.insert(
        invocation_id,
        InFlightHostCall {
            module,
            function,
            plugin: plugin.to_owned(),
            started,
        },
    );
    debug!(
        host_fn = function,
        host_module = module,
        plugin = plugin,
        invocation_id = invocation_id,
        "host call entered"
    );
    started
}

/// Record that `invocation_id` has left a host call.
///
/// `completed` distinguishes a call that returned from one whose future was
/// dropped before it could. Cancellation is not hypothetical: when a cron run's
/// HTTP client disconnects, the whole request future is dropped and every host
/// call under it goes with it. Recording that as an ordinary return would leave
/// a stale registry entry claiming the guest is still inside a call it has long
/// since been torn out of.
fn leave(
    invocation_id: u64,
    module: &'static str,
    function: &'static str,
    plugin: &str,
    started: Instant,
    completed: bool,
) {
    IN_FLIGHT.remove(&invocation_id);
    let elapsed_ms = started.elapsed().as_millis();
    if !completed {
        debug!(
            host_fn = function,
            host_module = module,
            plugin = plugin,
            invocation_id = invocation_id,
            elapsed_ms = elapsed_ms,
            "host call cancelled before it returned"
        );
    } else if elapsed_ms >= SLOW_HOST_CALL_MS {
        warn!(
            host_fn = function,
            host_module = module,
            plugin = plugin,
            invocation_id = invocation_id,
            elapsed_ms = elapsed_ms,
            "host call was slow"
        );
    } else {
        debug!(
            host_fn = function,
            host_module = module,
            plugin = plugin,
            invocation_id = invocation_id,
            elapsed_ms = elapsed_ms,
            "host call returned"
        );
    }
}

/// RAII record of one host call, for synchronous host functions.
///
/// Asynchronous host functions go through [`TracedLinker::func_wrap_async_traced`]
/// instead, which wraps the returned future; a synchronous closure cannot be
/// wrapped that way because `IntoFunc` is sealed over the closure's own arity,
/// so those register a guard at the top of the closure body.
pub(crate) struct HostCallGuard {
    invocation_id: u64,
    module: &'static str,
    function: &'static str,
    plugin: String,
    started: Instant,
    /// Whether the call reached its own end, as opposed to being dropped.
    completed: bool,
}

impl HostCallGuard {
    /// Open a guard for a synchronous host call.
    ///
    /// A synchronous call cannot be cancelled part way: once the closure is
    /// entered it runs to its end or traps, so the guard is born completed.
    pub(crate) fn new(
        caller: &Caller<'_, PluginState>,
        module: &'static str,
        function: &'static str,
    ) -> Self {
        let invocation_id = caller.data().invocation_id;
        let plugin = caller.data().plugin_name.clone();
        Self::open(invocation_id, module, function, plugin, true)
    }

    /// Open a guard from already-read identity, for the asynchronous path.
    ///
    /// `completed` starts `false` there: the future may be dropped before it
    /// finishes, and only [`Self::complete`] on the far side of the `await`
    /// proves it was not.
    fn open(
        invocation_id: u64,
        module: &'static str,
        function: &'static str,
        plugin: String,
        completed: bool,
    ) -> Self {
        let started = enter(invocation_id, module, function, &plugin);
        Self {
            invocation_id,
            module,
            function,
            plugin,
            started,
            completed,
        }
    }

    /// Mark the call as having returned under its own power.
    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for HostCallGuard {
    fn drop(&mut self) {
        leave(
            self.invocation_id,
            self.module,
            self.function,
            &self.plugin,
            self.started,
            self.completed,
        );
    }
}

/// `Linker` extension registering a host function with tracing around it.
pub(crate) trait TracedLinker {
    /// As [`Linker::func_wrap_async`], but the call is traced.
    ///
    /// # Errors
    ///
    /// Propagates whatever `Linker::func_wrap_async` returns.
    fn func_wrap_async_traced<F, Params, Args>(
        &mut self,
        module: &'static str,
        name: &'static str,
        func: F,
    ) -> wasmtime::Result<&mut Self>
    where
        F: for<'a> Fn(
                Caller<'a, PluginState>,
                Params,
            ) -> Box<dyn Future<Output = Args> + Send + 'a>
            + Send
            + Sync
            + 'static,
        Params: wasmtime::WasmTyList,
        Args: wasmtime::WasmRet + 'static;
}

impl TracedLinker for Linker<PluginState> {
    fn func_wrap_async_traced<F, Params, Args>(
        &mut self,
        module: &'static str,
        name: &'static str,
        func: F,
    ) -> wasmtime::Result<&mut Self>
    where
        F: for<'a> Fn(
                Caller<'a, PluginState>,
                Params,
            ) -> Box<dyn Future<Output = Args> + Send + 'a>
            + Send
            + Sync
            + 'static,
        Params: wasmtime::WasmTyList,
        Args: wasmtime::WasmRet + 'static,
    {
        self.func_wrap_async(module, name, move |caller, params| {
            // Read identity before the caller is handed to the wrapped closure.
            let invocation_id = caller.data().invocation_id;
            let plugin = caller.data().plugin_name.clone();
            // The guard, not a plain pair of calls: if this future is dropped
            // before it resolves, `Drop` still clears the in-flight entry.
            let mut guard = HostCallGuard::open(invocation_id, module, name, plugin, false);
            let inner = func(caller, params);
            Box::new(async move {
                let out = Box::into_pin(inner).await;
                guard.complete();
                drop(guard);
                out
            })
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn invocation_ids_are_unique() {
        let a = next_invocation_id();
        let b = next_invocation_id();
        assert_ne!(a, b, "each invocation must get its own id");
    }

    #[test]
    fn in_flight_reports_the_call_then_forgets_it() {
        let id = next_invocation_id();
        assert!(in_flight(id).is_none(), "nothing in flight before entry");

        let started = enter(id, "trovato:kernel/test", "spin", "testplugin");
        let seen = in_flight(id).expect("call is in flight after entry");
        assert_eq!(seen.function, "spin");
        assert_eq!(seen.plugin, "testplugin");
        assert_eq!(seen.qualified_name(), "trovato:kernel/test::spin");

        leave(
            id,
            "trovato:kernel/test",
            "spin",
            "testplugin",
            started,
            true,
        );
        assert!(in_flight(id).is_none(), "entry is cleared on return");
    }

    /// A host call whose future is dropped before it resolves must not leave a
    /// registry entry behind claiming the guest is still inside it.
    ///
    /// This is the cancellation a cron run actually suffers: the HTTP client
    /// disconnects, the request future is dropped, and every host call under it
    /// goes with it without ever reaching its own end.
    #[test]
    fn a_dropped_call_clears_its_entry() {
        let id = next_invocation_id();
        {
            let _guard = HostCallGuard::open(
                id,
                "trovato:kernel/test",
                "cancelled",
                "testplugin".to_string(),
                false,
            );
            assert!(
                in_flight(id).is_some(),
                "call is in flight inside the scope"
            );
        }
        assert!(
            in_flight(id).is_none(),
            "a dropped call must clear its in-flight entry, not leak it"
        );
    }
}
