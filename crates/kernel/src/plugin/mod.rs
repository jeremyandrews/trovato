//! Plugin system for Trovato.
//!
//! This module handles:
//! - Parsing plugin metadata from `.info.toml` files
//! - Loading and compiling WASM plugins
//! - Managing plugin dependencies
//! - Providing the runtime environment for plugin execution

mod dependency;
mod error;
mod info_parser;
mod runtime;

pub use dependency::{check_dependencies, resolve_load_order};
pub use error::PluginError;
pub use info_parser::{PluginInfo, TapConfig, TapOptions, KNOWN_TAPS};
pub use runtime::{CompiledPlugin, PluginConfig, PluginRuntime, PluginState};
