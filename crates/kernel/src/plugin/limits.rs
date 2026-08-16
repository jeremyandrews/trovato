//! Per-`Store` resource bounds for the plugin sandbox (WASM-4).
//!
//! This module is the single visible home for every resource limit the kernel
//! imposes on a plugin call: the per-`Store` linear-memory and table caps
//! (enforced through [`PluginResourceLimiter`], a [`wasmtime::ResourceLimiter`]),
//! the per-`Store` fuel budget, and the epoch-deadline budgets (named constants
//! shared by tap dispatch and `invoke`). Co-locating them means one place to
//! audit "how much can a plugin consume in a single call."
//!
//! # Limiter versus the pooling allocator
//!
//! Linear memory is bounded at two layers. The pooling allocator
//! ([`create_engine`](super::runtime)) pre-sizes a per-instance memory slab and
//! is the hard backstop. [`PluginResourceLimiter`] is the per-`Store`, per-call
//! cap that produces a *clean, attributed* error. The two are kept coherent at
//! engine creation — the pool slab is never smaller than the limiter cap — so
//! the limiter's precise error fires first and the plugin never bottoms out on
//! the allocator's opaque `memory.grow` failure. Defaults are deliberately equal
//! (64 MiB), so with the shipped configuration the two caps coincide.
//!
//! # Clean errors, not silent `-1`
//!
//! [`wasmtime::StoreLimits`] denies growth by returning `Ok(false)`, which makes
//! the guest's `memory.grow` yield `-1` with no host-side signal — the opaque
//! failure WASM-4 set out to remove. This limiter returns `Err` instead, raising
//! a trap so the call fails through the same logged `ExportCallError::Failed`
//! path as any other trap, with a message naming the plugin and the cap.

use tracing::warn;
use wasmtime::ResourceLimiter;

/// Default per-`Store` linear-memory cap: 64 MiB.
///
/// Deliberately equal to the pooling allocator's per-instance slab
/// (`max_memory_pages` × 64 KiB). The limiter is the *effective* cap and the
/// pool is the backstop; [`create_engine`](super::runtime) keeps the pool slab
/// ≥ this value so the limiter's clean error fires before the allocator's opaque
/// one.
pub const DEFAULT_MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;

/// Default per-`Store` table element cap (element-level, not pool slots).
pub const DEFAULT_MAX_TABLE_ELEMENTS: usize = 10_000;

/// Default per-`Store` linear-memory count. Every `wasm32-wasip1` plugin defines
/// exactly one memory (verified across the in-tree plugin set).
pub const DEFAULT_MAX_MEMORIES: usize = 1;

/// Default per-`Store` table count. Every in-tree plugin defines exactly one
/// funcref table.
pub const DEFAULT_MAX_TABLES: usize = 1;

/// Default per-`Store` instance count. Each call instantiates exactly one module
/// per `Store`; `invoke` runs its target in a *separate* child `Store`, so the
/// per-`Store` instance count stays 1 even for nested invocation.
pub const DEFAULT_MAX_INSTANCES: usize = 1;

/// Epoch budget (seconds) for a request-scoped tap. Unchanged from the historical
/// literal; extracted here so all resource bounds live in one place.
pub const TAP_EPOCH_DEADLINE_SECS: u64 = 10;

/// Epoch budget (seconds) for a background tap (see `BACKGROUND_TAPS`). Unchanged.
pub const BACKGROUND_TAP_EPOCH_DEADLINE_SECS: u64 = 150;

/// Epoch budget (seconds) for a plugin-to-plugin `invoke` target. Unchanged — the
/// same request-scoped deadline tap dispatch uses for non-background taps.
pub const INVOKE_EPOCH_DEADLINE_SECS: u64 = 10;

/// Whether per-`Store` fuel metering is enabled by default (off).
///
/// Epoch interruption remains the primary CPU bound. Fuel is a deterministic,
/// opt-in second bound; enabling it injects per-operator accounting into
/// generated code, so it stays off unless explicitly configured.
pub const DEFAULT_ENABLE_FUEL: bool = false;

/// Default per-`Store` fuel budget, consulted only when fuel is enabled.
pub const DEFAULT_FUEL_LIMIT: u64 = 10_000_000_000;

/// Resolved per-`Store` resource bounds, derived from
/// [`PluginConfig`](super::runtime::PluginConfig).
///
/// A cheap `Copy` snapshot carried on [`PluginRuntime`](super::runtime::PluginRuntime)
/// and stamped into each call's [`PluginResourceLimiter`]. Kept separate from
/// `PluginConfig` (which also holds pooling-allocator knobs) so the per-`Store`
/// hot path copies only the numbers it needs.
#[derive(Debug, Clone, Copy)]
pub struct ResourceLimits {
    /// Maximum linear-memory bytes a single `Store` may grow to.
    pub max_memory_bytes: usize,
    /// Maximum elements a single `Store` may grow a table to.
    pub max_table_elements: usize,
    /// Maximum linear memories a single `Store` may create.
    pub max_memories: usize,
    /// Maximum tables a single `Store` may create.
    pub max_tables: usize,
    /// Maximum instances a single `Store` may create.
    pub max_instances: usize,
    /// Whether per-`Store` fuel metering is enabled.
    pub enable_fuel: bool,
    /// Per-`Store` fuel budget, consulted only when `enable_fuel` is set.
    pub fuel_limit: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
            max_table_elements: DEFAULT_MAX_TABLE_ELEMENTS,
            max_memories: DEFAULT_MAX_MEMORIES,
            max_tables: DEFAULT_MAX_TABLES,
            max_instances: DEFAULT_MAX_INSTANCES,
            enable_fuel: DEFAULT_ENABLE_FUEL,
            fuel_limit: DEFAULT_FUEL_LIMIT,
        }
    }
}

/// Per-`Store` [`wasmtime::ResourceLimiter`] that bounds a single plugin call's
/// linear-memory and table growth and fails the call cleanly on breach.
///
/// Attached to every plugin `Store` at creation via `store.limiter(...)`. On a
/// disallowed growth it returns `Err`, which wasmtime raises as a trap — so the
/// call fails through the same logged `ExportCallError::Failed` path as any other
/// trap, attributed to the plugin, and the kernel stays healthy for the next
/// call. The `warn!` here guarantees a kernel-side log on every breach regardless
/// of which caller (tap dispatch or `invoke`) drove the call.
///
/// The instance/table/memory *count* caps ([`ResourceLimiter::instances`],
/// [`ResourceLimiter::tables`], [`ResourceLimiter::memories`]) are read once when
/// the limiter is attached and enforced during instantiation.
#[derive(Debug)]
pub struct PluginResourceLimiter {
    /// The numeric bounds this limiter enforces.
    limits: ResourceLimits,
    /// Plugin name, carried for log attribution and the trap message.
    plugin_name: String,
}

impl PluginResourceLimiter {
    /// Build a limiter for a single plugin call.
    pub fn new(limits: ResourceLimits, plugin_name: String) -> Self {
        Self {
            limits,
            plugin_name,
        }
    }
}

impl ResourceLimiter for PluginResourceLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.limits.max_memory_bytes {
            warn!(
                plugin = %self.plugin_name,
                desired_bytes = desired,
                limit_bytes = self.limits.max_memory_bytes,
                "plugin exceeded per-store linear-memory limit"
            );
            return Err(wasmtime::Error::msg(format!(
                "memory-limit-exceeded: plugin '{}' requested {desired} bytes > {} byte cap",
                self.plugin_name, self.limits.max_memory_bytes
            )));
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.limits.max_table_elements {
            warn!(
                plugin = %self.plugin_name,
                desired_elements = desired,
                limit_elements = self.limits.max_table_elements,
                "plugin exceeded per-store table-element limit"
            );
            return Err(wasmtime::Error::msg(format!(
                "table-limit-exceeded: plugin '{}' requested {desired} elements > {} element cap",
                self.plugin_name, self.limits.max_table_elements
            )));
        }
        Ok(true)
    }

    fn instances(&self) -> usize {
        self.limits.max_instances
    }

    fn tables(&self) -> usize {
        self.limits.max_tables
    }

    fn memories(&self) -> usize {
        self.limits.max_memories
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn default_limits_match_documented_constants() {
        let d = ResourceLimits::default();
        assert_eq!(d.max_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(d.max_table_elements, 10_000);
        assert_eq!(d.max_memories, 1);
        assert_eq!(d.max_tables, 1);
        assert_eq!(d.max_instances, 1);
        assert!(!d.enable_fuel, "fuel must be off by default");
    }

    #[test]
    fn memory_growth_under_cap_is_allowed() {
        let mut lim = PluginResourceLimiter::new(ResourceLimits::default(), "p".to_string());
        // 1 MiB desired, well under the 64 MiB default cap.
        assert!(lim.memory_growing(0, 1 << 20, None).unwrap());
    }

    #[test]
    fn memory_growth_over_cap_errors_with_attribution() {
        let limits = ResourceLimits {
            max_memory_bytes: 2 << 20, // 2 MiB
            ..ResourceLimits::default()
        };
        let mut lim = PluginResourceLimiter::new(limits, "greedy".to_string());
        let err = lim
            .memory_growing(0, 4 << 20, None)
            .expect_err("growth beyond cap must error, not return Ok(false)")
            .to_string();
        assert!(err.contains("memory-limit-exceeded"), "got: {err}");
        assert!(err.contains("greedy"), "error must name the plugin: {err}");
    }

    #[test]
    fn table_growth_over_cap_errors_with_attribution() {
        let limits = ResourceLimits {
            max_table_elements: 5,
            ..ResourceLimits::default()
        };
        let mut lim = PluginResourceLimiter::new(limits, "tabby".to_string());
        let err = lim
            .table_growing(0, 6, None)
            .expect_err("table growth beyond cap must error")
            .to_string();
        assert!(err.contains("table-limit-exceeded"), "got: {err}");
        assert!(err.contains("tabby"), "error must name the plugin: {err}");
    }

    #[test]
    fn count_caps_reflect_configured_limits() {
        let limits = ResourceLimits {
            max_instances: 1,
            max_tables: 1,
            max_memories: 1,
            ..ResourceLimits::default()
        };
        let lim = PluginResourceLimiter::new(limits, "p".to_string());
        assert_eq!(lim.instances(), 1);
        assert_eq!(lim.tables(), 1);
        assert_eq!(lim.memories(), 1);
    }
}
