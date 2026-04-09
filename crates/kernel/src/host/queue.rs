//! Queue host functions for WASM plugins.
//!
//! Provides `queue_push` so plugins can enqueue work from `tap_cron`.
//! The kernel's cron task drains the queue and calls `tap_queue_worker`
//! on the owning plugin for each item.

use anyhow::Result;
use tracing::warn;
use wasmtime::Linker;

use super::read_string_from_memory;
use crate::plugin::{PluginState, WasmtimeExt};

/// Register queue host functions.
pub fn register_queue_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // push(queue_name_ptr, queue_name_len, payload_ptr, payload_len) -> i32
    //
    // Returns 0 on success, negative error code on failure.
    // The plugin_name is injected from PluginState so plugins cannot
    // impersonate each other.
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
                let created_at = chrono::Utc::now().timestamp();

                let result = sqlx::query(
                    r#"
                    INSERT INTO plugin_queue (plugin_name, queue_name, payload, created_at)
                    VALUES ($1, $2, $3, $4)
                    "#,
                )
                .bind(&plugin_name)
                .bind(&queue_name)
                .bind(&payload)
                .bind(created_at)
                .execute(&db)
                .await;

                match result {
                    Ok(_) => 0i32,
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

    Ok(())
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
