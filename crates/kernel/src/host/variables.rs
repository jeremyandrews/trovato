//! Variables host functions for WASM plugins.
//!
//! Provides persistent key-value configuration storage via the
//! `site_config` table. Variable keys are namespaced by plugin name
//! to prevent collisions: `plugin.{plugin_name}.{key}`.
//!
//! # An unset variable is indistinguishable from a set one
//!
//! `get` takes a default and returns a string, so a plugin reading a variable
//! that does not exist gets its default back and cannot tell the difference. The
//! namespacing makes that easy to hit by accident: a plugin asking for another
//! plugin's setting is asking for `plugin.{its own name}.{that key}`, which will
//! never exist, so the read silently succeeds with the default forever.
//!
//! The kernel cannot fix that half of it here. The WIT declares
//! `get: func(name: string, default-value: string) -> string` — no `option`, no
//! `result` — and the SDK maps every negative return to `Err(code)` meaning "host
//! function failure". Returning a sentinel for "unset" would make an ordinary
//! unset variable look like a host failure to every plugin already compiled, so
//! it is a contract change, not an additive one, and the plugin contract is
//! frozen before 1.0.
//!
//! What is done instead: the **host** says so, at warn level, the first time each
//! key misses in the life of the process. The information reaches the operator
//! and the plugin author through the log rather than through the return value.
//! Giving the SDK a real unset signal is recorded for a later minor.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use dashmap::DashSet;
use tracing::warn;
use trovato_sdk::host_errors;
use wasmtime::Linker;

use super::{read_string_from_memory, write_string_to_memory};
use crate::models::SiteConfig;
use crate::plugin::{PluginState, WasmtimeExt};

/// Namespaced keys already reported missing, so each is logged once per process.
static REPORTED_MISSES: LazyLock<DashSet<String>> = LazyLock::new(DashSet::new);

/// Whether the "too many distinct keys" notice has been logged.
static MISS_TRACKING_SATURATED: AtomicBool = AtomicBool::new(false);

/// Most distinct missing keys tracked before reporting stops.
///
/// The set exists to suppress repeats, and a plugin reading keys built from
/// request data would otherwise grow it without bound. At the cap tracking stops
/// entirely, with one notice saying so: an unbounded set in a long-running
/// process is a worse problem than an under-reported warning.
const MAX_TRACKED_MISSES: usize = 1024;

/// Whether this miss should be logged — true only the first time for a key.
///
/// Takes the set and the saturation flag rather than reading the statics, so the
/// once-per-key rule and the cap can each be tested on their own state. The
/// process-wide pair lives in [`should_report_miss`].
fn should_report_miss_in(seen: &DashSet<String>, saturated: &AtomicBool, db_key: &str) -> bool {
    if seen.contains(db_key) {
        return false;
    }
    if seen.len() >= MAX_TRACKED_MISSES {
        if !saturated.swap(true, Ordering::Relaxed) {
            warn!(
                tracked = MAX_TRACKED_MISSES,
                "too many distinct unset plugin variables to keep tracking; \
                 further unset-variable warnings are suppressed"
            );
        }
        return false;
    }
    seen.insert(db_key.to_string())
}

/// Whether this miss should be logged, against the process-wide record.
fn should_report_miss(db_key: &str) -> bool {
    should_report_miss_in(&REPORTED_MISSES, &MISS_TRACKING_SATURATED, db_key)
}

/// Say, once per key per process, that a plugin read a variable that is not set.
///
/// Names the namespaced key as well as the one the plugin asked for, because the
/// gap between the two is the mistake this usually catches.
fn report_unset_variable(plugin_name: &str, name: &str, db_key: &str) {
    if !should_report_miss(db_key) {
        return;
    }
    warn!(
        plugin = %plugin_name,
        key = %name,
        resolved_key = %db_key,
        "plugin read an unset variable and received its default; a variable is \
         namespaced to the plugin that reads it, so this key cannot be another \
         plugin's setting"
    );
}

/// Register variables host functions.
pub fn register_variables_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // get(name, default) -> string (bytes written or 0)
    linker
        .func_wrap_async(
            "trovato:kernel/variables",
            "get",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (name_ptr, name_len, default_ptr, default_len, out_ptr, out_max_len): (
                i32,
                i32,
                i32,
                i32,
                i32,
                i32,
            )| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return 0;
                    };

                    let Ok(name) = read_string_from_memory(&memory, &caller, name_ptr, name_len)
                    else {
                        return 0;
                    };

                    let default_value =
                        read_string_from_memory(&memory, &caller, default_ptr, default_len)
                            .unwrap_or_default();

                    // Namespace the key by plugin name
                    let plugin_name = caller.data().plugin_name.clone();
                    let db_key = format!("plugin.{plugin_name}.{name}");

                    // Try to read from site_config via DB
                    let value = if let Some(services) = caller.data().request.services() {
                        let pool = services.db.clone();
                        match SiteConfig::get(&pool, &db_key).await {
                            Ok(Some(v)) => match v {
                                serde_json::Value::String(s) => s,
                                other => other.to_string(),
                            },
                            Ok(None) => {
                                report_unset_variable(&plugin_name, &name, &db_key);
                                default_value.clone()
                            }
                            Err(e) => {
                                warn!(
                                    plugin = %plugin_name,
                                    key = %name,
                                    error = %e,
                                    "failed to read variable"
                                );
                                default_value.clone()
                            }
                        }
                    } else {
                        default_value.clone()
                    };

                    write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &value)
                        .unwrap_or(0)
                })
            },
        )
        .into_anyhow()?;

    // set(name, value) -> result (0 = success, negative = error)
    linker
        .func_wrap_async(
            "trovato:kernel/variables",
            "set",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (name_ptr, name_len, value_ptr, value_len): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };

                    let Ok(name) = read_string_from_memory(&memory, &caller, name_ptr, name_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };

                    let Ok(value) = read_string_from_memory(&memory, &caller, value_ptr, value_len)
                    else {
                        return host_errors::ERR_PARAM2_OR_OUTPUT;
                    };

                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };

                    let plugin_name = caller.data().plugin_name.clone();
                    let db_key = format!("plugin.{plugin_name}.{name}");
                    let pool = services.db.clone();

                    match SiteConfig::set(&pool, &db_key, serde_json::Value::String(value)).await {
                        Ok(()) => {
                            // Amendment α: a plugin config write may change a
                            // `tap_field_access` plugin's field rules. Flush the
                            // shared field-access decision cache so the change
                            // takes effect on the next request instead of riding
                            // the ≤5-minute TTL. Blunt whole-cache flush: config
                            // writes are rare and moka coalesces the refill.
                            services.field_access_cache.invalidate_all();
                            0
                        }
                        Err(e) => {
                            warn!(
                                plugin = %plugin_name,
                                key = %name,
                                error = %e,
                                "failed to write variable"
                            );
                            host_errors::ERR_SQL_FAILED
                        }
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

    /// A unique key per test run: `REPORTED_MISSES` is process-wide and every
    /// test in this binary shares it.
    fn unique_key(tag: &str) -> String {
        format!("plugin.test.{tag}.{}", uuid::Uuid::now_v7())
    }

    /// The defect made observable: a miss used to be silent. It is now reported,
    /// and reported once.
    #[test]
    fn a_miss_is_reported_once_per_key() {
        let key = unique_key("once");
        assert!(
            should_report_miss(&key),
            "the first miss on a key must be reported"
        );
        assert!(
            !should_report_miss(&key),
            "a repeat read of the same unset key must not log again"
        );
        assert!(
            !should_report_miss(&key),
            "and must keep not logging, however many times it is read"
        );
    }

    /// Two keys are two reports: suppressing a repeat must not suppress a
    /// different key's first miss.
    #[test]
    fn distinct_keys_are_reported_separately() {
        let first = unique_key("a");
        let second = unique_key("b");
        assert!(should_report_miss(&first));
        assert!(should_report_miss(&second));
        assert!(!should_report_miss(&first));
    }

    /// The reason the key is namespaced is the mistake this catches: one plugin
    /// reading another's setting resolves to a key that cannot exist. The two
    /// plugins' reads are distinct keys, so both are reported.
    #[test]
    fn the_same_name_under_two_plugins_is_two_keys() {
        let name = uuid::Uuid::now_v7();
        let mine = format!("plugin.mine.{name}");
        let theirs = format!("plugin.theirs.{name}");
        assert!(should_report_miss(&mine));
        assert!(
            should_report_miss(&theirs),
            "namespacing is what makes these different reads"
        );
    }

    /// Tracking is bounded: a plugin building keys from request data must not
    /// grow the set without limit in a long-running process.
    ///
    /// On its own set, not the process-wide one — saturating that would suppress
    /// the reports every other test in this binary is asserting.
    #[test]
    fn miss_tracking_is_bounded() {
        // A zero cap would disable reporting entirely; the loop below asserts
        // the cap's real behaviour, so there is nothing to check about the
        // constant itself beyond that it is what the loop runs to.
        let seen = DashSet::new();
        let saturated = AtomicBool::new(false);

        for i in 0..MAX_TRACKED_MISSES {
            assert!(
                should_report_miss_in(&seen, &saturated, &format!("plugin.flood.{i}")),
                "every key up to the cap is reported"
            );
        }
        assert_eq!(seen.len(), MAX_TRACKED_MISSES);
        assert!(!saturated.load(Ordering::Relaxed), "not yet saturated");

        assert!(
            !should_report_miss_in(&seen, &saturated, "plugin.flood.one-too-many"),
            "past the cap, reporting stops rather than growing the set"
        );
        assert_eq!(
            seen.len(),
            MAX_TRACKED_MISSES,
            "the set must never exceed its cap"
        );
        assert!(saturated.load(Ordering::Relaxed), "and says so, once");
    }

    #[test]
    fn register_variables_succeeds() {
        let config = wasmtime::Config::new();
        let engine = Engine::new(&config).expect("valid engine config");
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_variables_functions(&mut linker);
        assert!(result.is_ok());
    }
}
