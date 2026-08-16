//! Queue host functions for WASM plugins.
//!
//! Provides `push` so plugins can enqueue work from `tap_cron`, and the additive
//! `enqueue` (P11d / D-48) which carries optional `priority`/`delay`. The
//! kernel's drain (cron or the resident runner) pops jobs and calls
//! `tap_queue_worker` on the owning plugin for each item, applying v2
//! retry/backoff/dead-letter semantics regardless of which entry point enqueued
//! the job.

use anyhow::Result;
use tracing::warn;
use wasmtime::Linker;

use super::read_string_from_memory;
use crate::plugin::{PluginState, WasmtimeExt};
use trovato_sdk::host_errors;

/// Insert a job into `plugin_queue` with an explicit priority and first-attempt
/// time. Shared by `push` (priority 0, no delay) and `enqueue`.
///
/// Returns `Ok(())` on success or `Err(())` if the insert failed (the caller
/// maps that to its own error code); the plugin name is injected by the caller
/// so plugins cannot enqueue under another plugin's identity.
async fn insert_job(
    db: &sqlx::PgPool,
    plugin_name: &str,
    queue_name: &str,
    payload: &serde_json::Value,
    priority: i32,
    next_attempt_at: i64,
) -> std::result::Result<(), sqlx::Error> {
    let created_at = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"
        INSERT INTO plugin_queue
            (plugin_name, queue_name, payload, created_at, priority, next_attempt_at)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(plugin_name)
    .bind(queue_name)
    .bind(payload)
    .bind(created_at)
    .bind(priority)
    .bind(next_attempt_at)
    .execute(db)
    .await
    .map(|_| ())
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
    linker.func_wrap_async(
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

                match insert_job(&db, &plugin_name, &queue_name, &payload, 0, 0).await {
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
        .func_wrap_async(
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
}
