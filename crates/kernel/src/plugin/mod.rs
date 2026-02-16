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
mod info_parser;
pub mod migration;
pub mod runtime;
pub mod status;

pub use dependency::{check_dependencies, resolve_load_order};
pub use error::PluginError;
pub use info_parser::{KNOWN_TAPS, MigrationConfig, PluginInfo, TapConfig, TapOptions};
pub use runtime::{CompiledPlugin, PluginConfig, PluginRuntime, PluginState};
