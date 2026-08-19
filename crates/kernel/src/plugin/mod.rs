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
pub mod db_policy;
mod dependency;
mod error;
pub mod gate;
mod info_parser;
pub mod limits;
pub mod migration;
pub mod runtime;
pub mod status;

pub use db_policy::DbPolicy;
pub use dependency::{check_dependencies, resolve_load_order};
pub use error::PluginError;
pub use info_parser::{
    KNOWN_HOST_INTERFACES, KNOWN_TAPS, MigrationConfig, PluginCapabilities, PluginInfo,
    RecordTypeDecl, TapConfig, TapOptions,
};
pub(crate) use runtime::WasmtimeExt;
pub use runtime::{CompiledPlugin, PluginConfig, PluginLoadError, PluginRuntime, PluginState};

/// Current kernel plugin API version.
///
/// Plugins declare an `api_version` in their `.info.toml`. At enable time,
/// the kernel enforces: plugin MAJOR == kernel MAJOR, plugin MINOR <= kernel MINOR
/// (see [`crate::plugin::PluginInfo::check_api_compatibility`]).
///
/// Increment MINOR when new host functions or taps are added (backward-compatible).
/// Increment MAJOR when host functions are removed or signatures change (breaking).
///
/// **This tuple is not independent.** It is the project version with the patch
/// component dropped: kernel `0.101.0` means API `(0, 101)`, and at `1.0.0` it
/// becomes `(1, 0)`. There is one version for the whole project and everything
/// moves together; see `docs/design/Versioning.md`.
///
/// **The contract is frozen even though the number is pre-1.0.** The plugin
/// boundary (the WIT surface, the SDK crate, the manifest semantics, the error
/// vocabularies) was frozen before the first public release and does not change
/// before 1.0. `cargo-semver-checks` guards the SDK crate in CI and the WIT is
/// kept truthful by audit. Under Cargo's 0.x rules a break would be permitted by a
/// MINOR bump, so the discipline is a policy rather than something the tooling
/// can enforce on its own: do not break it. At `1.0.0` the tooling and the
/// policy agree again, and a break requires a MAJOR bump.
///
/// **A `(0, x)` manifest from before the freeze is not what it appears to be.**
/// The `major == major` rule means a manifest declaring an old pre-freeze
/// `api_version` such as `"0.2"` now passes the version check, because the
/// kernel major is also `0`. That check is a compatibility gate, not a
/// provenance check: nothing built against the pre-freeze API was ever
/// released, so no such plugin exists in the wild. Build against the current
/// SDK and declare the current `api_version`.
pub const KERNEL_API_VERSION: (u32, u32) = (0, 101);
