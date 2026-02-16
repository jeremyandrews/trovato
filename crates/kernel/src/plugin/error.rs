//! Plugin system error types with clear, actionable messages.
//!
//! All errors include the plugin name and relevant context to help
//! developers quickly identify and fix issues.

use thiserror::Error;

/// Errors that can occur during plugin loading and execution.
#[derive(Debug, Error)]
pub enum PluginError {
    /// Plugin directory is missing the .info.toml manifest file.
    #[error("plugin '{plugin}': no .info.toml manifest found in {path}")]
    MissingManifest { plugin: String, path: String },

    /// Multiple .info.toml files found in plugin directory.
    #[error("plugin directory '{path}': multiple .info.toml files found, expected exactly one")]
    MultipleManifests { path: String },

    /// The .info.toml file could not be parsed.
    #[error("plugin '{plugin}': failed to parse manifest: {details}")]
    InvalidManifest { plugin: String, details: String },

    /// Plugin declares a tap that doesn't exist.
    #[error("plugin '{plugin}': declares unknown tap '{tap}'. Valid taps: {valid_taps}")]
    UnknownTap {
        plugin: String,
        tap: String,
        valid_taps: String,
    },

    /// Plugin's WASM file is missing.
    #[error(
        "plugin '{plugin}': WASM file not found at {expected_path}. Build with: cargo build -p {plugin} --target wasm32-wasip1 --release"
    )]
    MissingWasm {
        plugin: String,
        expected_path: String,
    },

    /// WASM module failed to compile.
    #[error("plugin '{plugin}': WASM compilation failed: {details}")]
    CompilationFailed { plugin: String, details: String },

    /// Plugin depends on another plugin that isn't loaded.
    #[error("plugin '{plugin}': depends on '{dependency}' which is not installed")]
    MissingDependency { plugin: String, dependency: String },

    /// Circular dependency detected.
    #[error("circular dependency detected: {cycle}")]
    CircularDependency { cycle: String },

    /// Plugin declares a tap but doesn't export it.
    #[error(
        "plugin '{plugin}': declares '{tap}' in manifest but doesn't export function '{export_name}'"
    )]
    MissingExport {
        plugin: String,
        tap: String,
        export_name: String,
    },

    /// Plugin's tap export has wrong signature.
    #[error(
        "plugin '{plugin}': tap '{tap}' has wrong signature. Expected (i32, i32) -> i64, got {actual}"
    )]
    WrongSignature {
        plugin: String,
        tap: String,
        actual: String,
    },

    /// Plugin panicked during tap execution.
    #[error("plugin '{plugin}': tap '{tap}' panicked: {message}")]
    PluginPanic {
        plugin: String,
        tap: String,
        message: String,
    },

    /// Plugin tap returned an error.
    #[error("plugin '{plugin}': tap '{tap}' returned error: {message}")]
    TapError {
        plugin: String,
        tap: String,
        message: String,
    },

    /// General instantiation failure.
    #[error("plugin '{plugin}': failed to instantiate: {details}")]
    InstantiationFailed { plugin: String, details: String },

    /// Migration SQL file not found on disk.
    #[error("plugin '{plugin}': migration file not found: {path}")]
    MigrationFileNotFound { plugin: String, path: String },

    /// Migration SQL execution failed.
    #[error("plugin '{plugin}': migration '{migration}' failed: {details}")]
    MigrationFailed {
        plugin: String,
        migration: String,
        details: String,
    },

    /// Migration declares a dependency on a plugin that isn't available.
    #[error("plugin '{plugin}': migration depends on '{dependency}' which is not available")]
    MigrationDependencyMissing { plugin: String, dependency: String },
}

impl PluginError {
    /// Create a missing manifest error.
    pub fn missing_manifest(path: impl Into<String>) -> Self {
        let path = path.into();
        let plugin = std::path::Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        Self::MissingManifest { plugin, path }
    }

    /// Create an unknown tap error.
    pub fn unknown_tap(plugin: impl Into<String>, tap: impl Into<String>, valid: &[&str]) -> Self {
        Self::UnknownTap {
            plugin: plugin.into(),
            tap: tap.into(),
            valid_taps: valid.join(", "),
        }
    }

    /// Create a missing WASM error.
    pub fn missing_wasm(plugin: impl Into<String>, path: impl Into<String>) -> Self {
        Self::MissingWasm {
            plugin: plugin.into(),
            expected_path: path.into(),
        }
    }

    /// Create a missing export error.
    pub fn missing_export(plugin: impl Into<String>, tap: impl Into<String>) -> Self {
        let tap_str = tap.into();
        let export_name = tap_str.replace('_', "-");
        Self::MissingExport {
            plugin: plugin.into(),
            tap: tap_str,
            export_name,
        }
    }

    /// Create a plugin panic error.
    pub fn panic(
        plugin: impl Into<String>,
        tap: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::PluginPanic {
            plugin: plugin.into(),
            tap: tap.into(),
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages_are_actionable() {
        let err = PluginError::missing_wasm("blog", "/plugins/blog/blog.wasm");
        let msg = err.to_string();
        assert!(msg.contains("blog"));
        assert!(msg.contains("cargo build"));
        assert!(msg.contains("wasm32-wasip1"));
    }

    #[test]
    fn unknown_tap_lists_valid_options() {
        let err = PluginError::unknown_tap("blog", "tap_invalid", &["tap_item_info", "tap_menu"]);
        let msg = err.to_string();
        assert!(msg.contains("tap_item_info"));
        assert!(msg.contains("tap_menu"));
    }

    #[test]
    fn missing_export_shows_export_name() {
        let err = PluginError::missing_export("blog", "tap_item_view");
        let msg = err.to_string();
        assert!(msg.contains("tap-item-view")); // WASM export name
    }
}
