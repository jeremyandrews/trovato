//! Queue host functions for WASM plugins.
//!
//! Provides `push` so plugins can enqueue work from `tap_cron`, and the additive
//! `enqueue` (P11d / D-48) which carries optional `priority`/`delay`. The
//! kernel's drain (cron or the resident runner) pops jobs and calls
//! `tap_queue_worker` on the owning plugin for each item, applying v2
//! retry/backoff/dead-letter semantics regardless of which entry point enqueued
//! the job.
//!
//! A queue can be shared: its owner lists, in its `tap_queue_info` entry, the
//! plugins allowed to push to it, and those plugins push with `push-to`. The row
//! is stored under the owner, so the owner's drain, claim and width apply
//! unchanged, and the kernel records who pushed it; see [`register_queue_functions`].

use anyhow::Result;
use tracing::warn;
use wasmtime::Linker;

use super::trace::TracedLinker;

use super::read_string_from_memory;
use crate::plugin::{PluginRuntime, PluginState, WasmtimeExt};
use crate::tap::{RequestState, instantiate_and_call_export};
use trovato_sdk::host_errors;

/// Insert a job into `plugin_queue` with an explicit priority and first-attempt
/// time. Shared by `push` (priority 0, no delay), `enqueue` and `push-to`.
///
/// Returns `Ok(())` on success or the database error (the caller maps that to
/// its own error code). Both names are injected by the caller from the calling
/// plugin's state, so a plugin cannot enqueue under another plugin's identity:
/// `plugin_name` is the owner whose worker runs the job, and `sender`, set only
/// by `push-to`, is the plugin that pushed it.
async fn insert_job(
    db: &sqlx::PgPool,
    plugin_name: &str,
    queue_name: &str,
    payload: &serde_json::Value,
    priority: i32,
    next_attempt_at: i64,
    sender: Option<&str>,
) -> std::result::Result<(), sqlx::Error> {
    let created_at = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"
        INSERT INTO plugin_queue
            (plugin_name, queue_name, payload, created_at, priority, next_attempt_at, sender)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(plugin_name)
    .bind(queue_name)
    .bind(payload)
    .bind(created_at)
    .bind(priority)
    .bind(next_attempt_at)
    .bind(sender)
    .execute(db)
    .await
    .map(|_| ())
}

/// Whether `owner`'s `tap_queue_info` output lets `pusher` push to `queue`.
///
/// The owner must declare the queue, and either be the pusher or list it in the
/// entry's `shared_with`. Anything else, a malformed declaration included, is a
/// refusal: a queue is private unless its owner says otherwise.
fn declaration_allows(output: &str, queue: &str, owner: &str, pusher: &str) -> bool {
    let Ok(serde_json::Value::Array(entries)) = serde_json::from_str(output) else {
        return false;
    };
    entries
        .iter()
        .filter(|e| e.get("name").and_then(serde_json::Value::as_str) == Some(queue))
        .any(|e| {
            pusher == owner
                || e.get("shared_with")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|list| list.iter().any(|p| p.as_str() == Some(pusher)))
        })
}

/// Ask `owner` whether `pusher` may push to its `queue`, by dispatching the
/// owner's `tap_queue_info`, the declaration the drain reads every cycle.
async fn may_push_to(
    runtime: &PluginRuntime,
    state: &RequestState,
    owner: &str,
    queue: &str,
    pusher: &str,
) -> bool {
    let Some(plugin) = runtime.get_plugin(owner) else {
        return false;
    };
    if !plugin
        .info
        .taps
        .implements
        .iter()
        .any(|t| t == "tap_queue_info")
    {
        return false;
    }
    let limits = runtime.limits();
    match instantiate_and_call_export(
        runtime,
        plugin.as_ref(),
        "tap_queue_info",
        "{}",
        state.clone(),
        crate::plugin::limits::TAP_EPOCH_DEADLINE_SECS,
        std::time::Duration::from_secs(limits.request_wallclock_ceiling_secs),
    )
    .await
    {
        Ok(output) => declaration_allows(&output, queue, owner, pusher),
        Err(_) => {
            warn!(owner = %owner, "push-to: the owner's tap_queue_info failed");
            false
        }
    }
}

/// Enqueue a job the **kernel itself** owns, under a reserved `plugin_name`
/// routed to a native drain arm rather than a WASM `tap_queue_worker` (P11f /
/// D-52). This is the kernel-internal producer seam: it reuses the same
/// [`insert_job`] insert path (and therefore the same queue-v2 columns and
/// retry/backoff/DLQ semantics) that `push`/`enqueue` use, without adding a
/// plugin-facing host function. The plugin-facing `queue.enqueue` surface
/// (errors `-34..-36`) is untouched.
///
/// `delay_secs` is clamped to non-negative, so a job can be deferred but never
/// scheduled into the past.
pub(crate) async fn enqueue_kernel_job(
    db: &sqlx::PgPool,
    plugin_name: &str,
    queue_name: &str,
    payload: &serde_json::Value,
    priority: i32,
    delay_secs: i64,
) -> std::result::Result<(), sqlx::Error> {
    let now = chrono::Utc::now().timestamp();
    let next_attempt_at = now.saturating_add(delay_secs.max(0));
    insert_job(
        db,
        plugin_name,
        queue_name,
        payload,
        priority,
        next_attempt_at,
        None,
    )
    .await
}

/// Register queue host functions.
pub fn register_queue_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // push(queue_name_ptr, queue_name_len, payload_ptr, payload_len) -> i32
    //
    // Returns 0 on success, negative error code on failure.
    // The plugin_name is injected from PluginState so plugins cannot
    // impersonate each other.
    //
    // ABI FROZEN (D-48): signature, error codes (-1..-5), and behavior are
    // byte-identical to v1 — priority 0, no delay. Do not modify; new options
    // ship via `enqueue` below.
    linker.func_wrap_async_traced(
        "trovato:kernel/queue",
        "push",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         (queue_name_ptr, queue_name_len, payload_ptr, payload_len): (i32, i32, i32, i32)| {
            Box::new(async move {
                let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return -1i32;
                };

                let Ok(queue_name) =
                    read_string_from_memory(&memory, &caller, queue_name_ptr, queue_name_len)
                else {
                    return -2i32;
                };

                let Ok(payload_json) =
                    read_string_from_memory(&memory, &caller, payload_ptr, payload_len)
                else {
                    return -3i32;
                };

                // Validate payload is well-formed JSON.
                let payload: serde_json::Value = match serde_json::from_str(&payload_json) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(error = %e, "queue_push: invalid payload JSON");
                        return -4i32;
                    }
                };

                let plugin_name = caller.data().plugin_name.clone();
                let db = caller.data().request.db().clone();

                match insert_job(&db, &plugin_name, &queue_name, &payload, 0, 0, None).await {
                    Ok(()) => 0i32,
                    Err(e) => {
                        warn!(
                            error = %e,
                            plugin = %plugin_name,
                            queue = %queue_name,
                            "queue_push: DB insert failed"
                        );
                        -5i32
                    }
                }
            })
        },
    ).into_anyhow()?;

    // enqueue(queue_ptr, queue_len, payload_ptr, payload_len, opts_ptr, opts_len) -> i32
    //
    // Additive companion to `push` (P11d / D-48). `opts` is a JSON object with
    // optional `priority` (i32, higher drains first) and `delay` (seconds to
    // defer the first attempt). Returns 0 on success or a negative host error
    // code. The plugin_name is injected so plugins cannot impersonate each
    // other; v2 retry/backoff/dead-letter semantics apply server-side.
    linker
        .func_wrap_async_traced(
            "trovato:kernel/queue",
            "enqueue",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (queue_name_ptr, queue_name_len, payload_ptr, payload_len, opts_ptr, opts_len): (
                i32,
                i32,
                i32,
                i32,
                i32,
                i32,
            )| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };

                    let Ok(queue_name) =
                        read_string_from_memory(&memory, &caller, queue_name_ptr, queue_name_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };

                    let Ok(payload_json) =
                        read_string_from_memory(&memory, &caller, payload_ptr, payload_len)
                    else {
                        return host_errors::ERR_PARAM2_OR_OUTPUT;
                    };

                    let Ok(opts_json) =
                        read_string_from_memory(&memory, &caller, opts_ptr, opts_len)
                    else {
                        return host_errors::ERR_PARAM3_READ;
                    };

                    // Validate payload is well-formed JSON.
                    let payload: serde_json::Value = match serde_json::from_str(&payload_json) {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(error = %e, "enqueue: invalid payload JSON");
                            return host_errors::ERR_QUEUE_INVALID_PAYLOAD;
                        }
                    };

                    // Parse options. An empty string is treated as no options.
                    let opts = if opts_json.trim().is_empty() {
                        EnqueueOpts::default()
                    } else {
                        match serde_json::from_str::<EnqueueOpts>(&opts_json) {
                            Ok(o) => o,
                            Err(e) => {
                                warn!(error = %e, "enqueue: invalid opts JSON");
                                return host_errors::ERR_QUEUE_INVALID_OPTS;
                            }
                        }
                    };

                    let now = chrono::Utc::now().timestamp();
                    // Clamp delay to non-negative; a job can be deferred, never
                    // scheduled into the past.
                    let next_attempt_at = now.saturating_add(opts.delay.max(0));

                    let plugin_name = caller.data().plugin_name.clone();
                    let db = caller.data().request.db().clone();

                    match insert_job(
                        &db,
                        &plugin_name,
                        &queue_name,
                        &payload,
                        opts.priority,
                        next_attempt_at,
                        None,
                    )
                    .await
                    {
                        Ok(()) => 0i32,
                        Err(e) => {
                            warn!(
                                error = %e,
                                plugin = %plugin_name,
                                queue = %queue_name,
                                "enqueue: DB insert failed"
                            );
                            host_errors::ERR_QUEUE_INSERT_FAILED
                        }
                    }
                })
            },
        )
        .into_anyhow()?;

    // push-to(owner_ptr, owner_len, queue_ptr, queue_len, payload_ptr, payload_len) -> i32
    //
    // Push to a queue another plugin owns. The owner must declare the queue in
    // its `tap_queue_info` and list the caller in that entry's `shared_with`;
    // otherwise `ERR_QUEUE_PUSH_REFUSED`. The row is stored under the owner, so
    // the owner's worker runs it with the owner's width, and the caller's name
    // is recorded as its sender from the caller's own state. Priority 0, no
    // delay, as `push`.
    linker
        .func_wrap_async_traced(
            "trovato:kernel/queue",
            "push-to",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (owner_ptr, owner_len, queue_ptr, queue_len, payload_ptr, payload_len): (
                i32,
                i32,
                i32,
                i32,
                i32,
                i32,
            )| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };
                    let Ok(owner) = read_string_from_memory(&memory, &caller, owner_ptr, owner_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };
                    let Ok(queue_name) =
                        read_string_from_memory(&memory, &caller, queue_ptr, queue_len)
                    else {
                        return host_errors::ERR_PARAM2_OR_OUTPUT;
                    };
                    let Ok(payload_json) =
                        read_string_from_memory(&memory, &caller, payload_ptr, payload_len)
                    else {
                        return host_errors::ERR_PARAM3_READ;
                    };
                    let payload: serde_json::Value = match serde_json::from_str(&payload_json) {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(error = %e, "push-to: invalid payload JSON");
                            return host_errors::ERR_QUEUE_INVALID_PAYLOAD;
                        }
                    };

                    let pusher = caller.data().plugin_name.clone();
                    let state = caller.data().request.clone();
                    let db = state.db().clone();
                    let runtime = state.services().and_then(|s| s.plugin_runtime().cloned());
                    let allowed = match &runtime {
                        Some(rt) => may_push_to(rt, &state, &owner, &queue_name, &pusher).await,
                        None => false,
                    };
                    if !allowed {
                        warn!(
                            pusher = %pusher,
                            owner = %owner,
                            queue = %queue_name,
                            "push-to refused: the owner does not share this queue with the caller"
                        );
                        return host_errors::ERR_QUEUE_PUSH_REFUSED;
                    }

                    match insert_job(&db, &owner, &queue_name, &payload, 0, 0, Some(&pusher)).await
                    {
                        Ok(()) => 0,
                        Err(e) => {
                            warn!(
                                error = %e,
                                pusher = %pusher,
                                owner = %owner,
                                queue = %queue_name,
                                "push-to: DB insert failed"
                            );
                            host_errors::ERR_QUEUE_INSERT_FAILED
                        }
                    }
                })
            },
        )
        .into_anyhow()?;

    Ok(())
}

/// Options accepted by the `enqueue` host function (P11d / D-48).
///
/// Both fields are optional; absent fields take their v1-equivalent defaults
/// (priority 0, no delay), so `enqueue(q, p, "{}")` behaves like `push(q, p)`.
#[derive(Debug, Default, serde::Deserialize)]
struct EnqueueOpts {
    /// Dispatch priority — higher values are drained first. Default 0.
    #[serde(default)]
    priority: i32,
    /// Seconds to defer the first attempt. Default 0 (eligible immediately).
    #[serde(default)]
    delay: i64,
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use wasmtime::Engine;

    #[test]
    fn register_queue_succeeds() {
        let config = wasmtime::Config::new();
        let engine = Engine::new(&config).unwrap();
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_queue_functions(&mut linker);
        assert!(result.is_ok());
    }

    const DECL: &str = r#"[
        {"name": "shared", "concurrency": 2, "shared_with": ["alpha", "beta"]},
        {"name": "private", "concurrency": 1}
    ]"#;

    #[test]
    fn a_listed_plugin_may_push_to_a_shared_queue() {
        assert!(declaration_allows(DECL, "shared", "owner", "alpha"));
        assert!(declaration_allows(DECL, "shared", "owner", "beta"));
    }

    #[test]
    fn everything_else_is_refused() {
        assert!(
            !declaration_allows(DECL, "shared", "owner", "gamma"),
            "not listed"
        );
        assert!(
            !declaration_allows(DECL, "private", "owner", "alpha"),
            "not shared"
        );
        assert!(
            !declaration_allows(DECL, "missing", "owner", "alpha"),
            "not declared"
        );
        assert!(!declaration_allows("not json", "shared", "owner", "alpha"));
        assert!(!declaration_allows(
            r#"{"name":"shared"}"#,
            "shared",
            "owner",
            "alpha"
        ));
    }

    #[test]
    fn the_owner_may_push_to_any_queue_it_declares() {
        assert!(declaration_allows(DECL, "private", "owner", "owner"));
        assert!(!declaration_allows(DECL, "missing", "owner", "owner"));
    }
}
