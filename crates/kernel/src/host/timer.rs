//! Timer host functions (B278): set, cancel and read the calling plugin's own
//! precise timers.
//!
//! The owner of every timer a plugin touches is the plugin's own name, taken
//! from [`PluginState`] and never from the call, so a plugin cannot see,
//! replace or cancel another owner's timer. The interface is gated like every
//! other: a plugin that does not declare `timer` under
//! `[capabilities] host_interfaces` has none of these functions linked, and
//! fails to load if it imports them. See `crate::timer`.

use anyhow::Result;
use serde::Serialize;
use sqlx::PgPool;
use wasmtime::Linker;

use super::trace::TracedLinker;
use super::{read_string_from_memory, write_string_to_memory};
use crate::plugin::{PluginState, WasmtimeExt};
use crate::timer::{self, Timer, TimerRefused, TimerRequest};
use trovato_sdk::host_errors;

/// A timer as its plugin sees it.
#[derive(Debug, Serialize)]
pub struct PluginTimer {
    pub name: String,
    /// Next run, Unix seconds.
    pub run_at: i64,
    pub repeat_secs: Option<i32>,
    pub payload: serde_json::Value,
    pub status: String,
    pub attempts: i32,
    /// Last fired, Unix seconds.
    pub last_fired_at: Option<i64>,
    pub last_error: Option<String>,
}

impl From<Timer> for PluginTimer {
    fn from(t: Timer) -> Self {
        Self {
            name: t.name,
            run_at: t.run_at.timestamp(),
            repeat_secs: t.repeat_secs,
            payload: t.payload,
            status: t.status,
            attempts: t.attempts,
            last_fired_at: t.last_fired_at.map(|at| at.timestamp()),
            last_error: t.last_error,
        }
    }
}

/// The error code for a refusal.
fn code(refused: &TimerRefused) -> i32 {
    match refused {
        TimerRefused::InvalidName | TimerRefused::InvalidTime | TimerRefused::NotFound => {
            host_errors::ERR_TIMER_INVALID
        }
        TimerRefused::PayloadTooLarge { .. } => host_errors::ERR_TIMER_PAYLOAD_TOO_LARGE,
        TimerRefused::RepeatTooShort { .. } => host_errors::ERR_TIMER_REPEAT_TOO_SHORT,
        TimerRefused::TooMany { .. } => host_errors::ERR_TIMER_LIMIT,
        TimerRefused::Storage(_) => host_errors::ERR_SQL_FAILED,
    }
}

/// `set` for `plugin`: `0`, or an error code. A refusal is logged with its
/// reason, naming the limit, so the operator sees why as well as the plugin.
pub async fn set_for(pool: &PgPool, plugin: &str, request_json: &str) -> i32 {
    let Ok(request) = serde_json::from_str::<TimerRequest>(request_json) else {
        return host_errors::ERR_PARAM_DESERIALIZE;
    };
    let settings = match timer::effective_settings(pool).await {
        Ok(settings) => settings,
        Err(e) => {
            tracing::warn!(plugin, error = %e, "timer.set: could not read the timer settings");
            return host_errors::ERR_SQL_FAILED;
        }
    };
    match timer::set(pool, plugin, &request, &settings).await {
        Ok(_) => 0,
        Err(refused) => {
            tracing::warn!(
                plugin,
                timer = %request.name,
                reason = %refused,
                "timer.set refused"
            );
            code(&refused)
        }
    }
}

/// `cancel` for `plugin`: `1` cancelled, `0` none, or an error code.
pub async fn cancel_for(pool: &PgPool, plugin: &str, name: &str) -> i32 {
    match timer::cancel(pool, plugin, name).await {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(e) => {
            tracing::warn!(plugin, timer = %name, error = %e, "timer.cancel failed");
            host_errors::ERR_SQL_FAILED
        }
    }
}

/// `get` for `plugin`: the timer as JSON, `None` when it has none of that
/// name, or an error code.
pub async fn get_for(pool: &PgPool, plugin: &str, name: &str) -> Result<Option<String>, i32> {
    match timer::get(pool, plugin, name).await {
        Ok(Some(t)) => serde_json::to_string(&PluginTimer::from(t))
            .map(Some)
            .map_err(|_| host_errors::ERR_SERIALIZE_FAILED),
        Ok(None) => Ok(None),
        Err(e) => {
            tracing::warn!(plugin, timer = %name, error = %e, "timer.get failed");
            Err(host_errors::ERR_SQL_FAILED)
        }
    }
}

/// Register the timer host functions.
pub fn register_timer_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    linker
        .func_wrap_async_traced(
            "trovato:kernel/timer",
            "set",
            |mut caller: wasmtime::Caller<'_, PluginState>, (req_ptr, req_len): (i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };
                    let Ok(request_json) =
                        read_string_from_memory(&memory, &caller, req_ptr, req_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };
                    let plugin = caller.data().plugin_name.clone();
                    let db = caller.data().request.db().clone();
                    set_for(&db, &plugin, &request_json).await
                })
            },
        )
        .into_anyhow()?;

    linker
        .func_wrap_async_traced(
            "trovato:kernel/timer",
            "cancel",
            |mut caller: wasmtime::Caller<'_, PluginState>, (name_ptr, name_len): (i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };
                    let Ok(name) = read_string_from_memory(&memory, &caller, name_ptr, name_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };
                    let plugin = caller.data().plugin_name.clone();
                    let db = caller.data().request.db().clone();
                    cancel_for(&db, &plugin, &name).await
                })
            },
        )
        .into_anyhow()?;

    linker
        .func_wrap_async_traced(
            "trovato:kernel/timer",
            "get",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (name_ptr, name_len, out_ptr, out_max_len): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };
                    let Ok(name) = read_string_from_memory(&memory, &caller, name_ptr, name_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };
                    let plugin = caller.data().plugin_name.clone();
                    let db = caller.data().request.db().clone();
                    match get_for(&db, &plugin, &name).await {
                        // Never a truncated timer: JSON cut short is no timer.
                        Ok(Some(json))
                            if json.len() > usize::try_from(out_max_len).unwrap_or(0) =>
                        {
                            host_errors::ERR_SDK_OUTPUT_BUFFER_EXCEEDED
                        }
                        Ok(Some(json)) => write_string_to_memory(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            &json,
                        )
                        .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT),
                        Ok(None) => 0,
                        Err(code) => code,
                    }
                })
            },
        )
        .into_anyhow()?;

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use wasmtime::Engine;

    #[test]
    fn register_timer_succeeds() {
        let engine = Engine::new(&wasmtime::Config::new()).unwrap();
        let mut linker: Linker<PluginState> = Linker::new(&engine);
        assert!(register_timer_functions(&mut linker).is_ok());
    }

    #[test]
    fn every_refusal_has_its_own_code() {
        assert_eq!(
            code(&TimerRefused::PayloadTooLarge { size: 5, max: 4 }),
            host_errors::ERR_TIMER_PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            code(&TimerRefused::RepeatTooShort { min: 60 }),
            host_errors::ERR_TIMER_REPEAT_TOO_SHORT
        );
        assert_eq!(
            code(&TimerRefused::TooMany {
                owner: "p".into(),
                max: 1
            }),
            host_errors::ERR_TIMER_LIMIT
        );
        assert_eq!(
            code(&TimerRefused::InvalidName),
            host_errors::ERR_TIMER_INVALID
        );
    }
}
