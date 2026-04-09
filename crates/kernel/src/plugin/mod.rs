//! Plugin system for Trovato.
//!
//! This module handles:
//! - Parsing plugin metadata from `.info.toml` files
//! - Loading and compiling WASM plugins
//! - Managing plugin dependencies
//! - Providing the runtime environment for plugin execution
//! - Plugin status tracking (enable/disable)
//! - Plugin-declared SQL migrations
//! - CLI commands for plugin management

pub mod cli;
mod dependency;
mod error;
pub mod gate;
mod info_parser;
pub mod migration;
pub mod runtime;
pub mod status;

pub use dependency::{check_dependencies, resolve_load_order};
pub use error::PluginError;
pub use info_parser::{KNOWN_TAPS, MigrationConfig, PluginInfo, TapConfig, TapOptions};
pub(crate) use runtime::WasmtimeExt;
pub use runtime::{CompiledPlugin, PluginConfig, PluginLoadError, PluginRuntime, PluginState};

/// Current kernel plugin API version.
///
/// Plugins declare an `api_version` in their `.info.toml`. At enable time,
/// the kernel enforces: plugin MAJOR == kernel MAJOR, plugin MINOR <= kernel MINOR.
///
/// Increment MINOR when new host functions or taps are added (backward-compatible).
/// Increment MAJOR when host functions are removed or signatures change (breaking).
pub const KERNEL_API_VERSION: (u32, u32) = (0, 2);
