//! Parser for plugin `.info.toml` manifest files.
//!
//! Each plugin has a `{name}.info.toml` file that declares metadata:
//! - name, version, description
//! - dependencies (other plugins that must load first)
//! - taps (which hook functions the plugin implements)

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Plugin metadata parsed from `.info.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginInfo {
    /// Plugin machine name (must match directory and file names).
    pub name: String,

    /// Human-readable description.
    pub description: String,

    /// Semantic version (e.g., "1.0.0").
    pub version: String,

    /// Other plugins this one depends on (loaded first).
    #[serde(default)]
    pub dependencies: Vec<String>,

    /// Tap configuration.
    #[serde(default)]
    pub taps: TapConfig,
}

/// Configuration for which taps a plugin implements.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TapConfig {
    /// List of tap function names this plugin exports.
    /// E.g., ["tap_item_info", "tap_item_view", "tap_menu"]
    #[serde(default)]
    pub implements: Vec<String>,

    /// Weight for ordering (lower = higher priority, default 0).
    #[serde(default)]
    pub weight: i32,

    /// Per-tap options (reserved for future use).
    #[serde(default)]
    pub options: HashMap<String, TapOptions>,
}

/// Per-tap configuration options.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TapOptions {
    // Reserved for future use (e.g., field filtering for filtered serialization)
}

/// Known tap names for validation.
pub const KNOWN_TAPS: &[&str] = &[
    // Lifecycle
    "tap_install",
    "tap_enable",
    "tap_disable",
    "tap_uninstall",
    // Content types
    "tap_item_info",
    // Item CRUD
    "tap_item_view",
    "tap_item_view_alter",
    "tap_item_insert",
    "tap_item_update",
    "tap_item_delete",
    "tap_item_access",
    // Categories
    "tap_categories_term_insert",
    "tap_categories_term_update",
    "tap_categories_term_delete",
    // Forms
    "tap_form_alter",
    "tap_form_validate",
    "tap_form_submit",
    // Routing & permissions
    "tap_menu",
    "tap_perm",
    // Theme
    "tap_theme",
    "tap_preprocess_item",
    // Search
    "tap_item_update_index",
    // Cron & queues
    "tap_cron",
    "tap_queue_info",
    "tap_queue_worker",
    // User
    "tap_user_login",
];

impl PluginInfo {
    /// Parse a plugin info file from the given path.
    pub fn parse(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read plugin info file: {}", path.display()))?;

        Self::parse_str(&content, path)
    }

    /// Parse plugin info from a TOML string.
    pub fn parse_str(content: &str, path: &Path) -> Result<Self> {
        let info: PluginInfo = toml::from_str(content).with_context(|| {
            format!(
                "failed to parse plugin info TOML at {}",
                path.display()
            )
        })?;

        info.validate(path)?;
        Ok(info)
    }

    /// Validate the parsed plugin info.
    fn validate(&self, path: &Path) -> Result<()> {
        // Validate name is not empty
        if self.name.is_empty() {
            anyhow::bail!(
                "plugin info at {} has empty 'name' field",
                path.display()
            );
        }

        // Validate version is not empty
        if self.version.is_empty() {
            anyhow::bail!(
                "plugin '{}' at {} has empty 'version' field",
                self.name,
                path.display()
            );
        }

        // Validate tap names are known
        for tap in &self.taps.implements {
            if !KNOWN_TAPS.contains(&tap.as_str()) {
                anyhow::bail!(
                    "plugin '{}' declares unknown tap '{}'. Known taps: {}",
                    self.name,
                    tap,
                    KNOWN_TAPS.join(", ")
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_info() {
        let toml = r#"
name = "blog"
description = "Provides a blog content type"
version = "1.0.0"
dependencies = ["item", "categories"]

[taps]
implements = ["tap_item_info", "tap_item_view", "tap_menu", "tap_perm"]
weight = 0
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert_eq!(info.name, "blog");
        assert_eq!(info.version, "1.0.0");
        assert_eq!(info.dependencies, vec!["item", "categories"]);
        assert_eq!(info.taps.implements.len(), 4);
        assert_eq!(info.taps.weight, 0);
    }

    #[test]
    fn parse_minimal_info() {
        let toml = r#"
name = "minimal"
description = "A minimal plugin"
version = "0.1.0"
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert_eq!(info.name, "minimal");
        assert!(info.dependencies.is_empty());
        assert!(info.taps.implements.is_empty());
        assert_eq!(info.taps.weight, 0);
    }

    #[test]
    fn reject_unknown_tap() {
        let toml = r#"
name = "bad"
description = "Bad plugin"
version = "1.0.0"

[taps]
implements = ["tap_unknown_function"]
"#;

        let result = PluginInfo::parse_str(toml, Path::new("test.toml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown tap"));
    }

    #[test]
    fn reject_empty_name() {
        let toml = r#"
name = ""
description = "Empty name"
version = "1.0.0"
"#;

        let result = PluginInfo::parse_str(toml, Path::new("test.toml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty 'name'"));
    }

    #[test]
    fn reject_empty_version() {
        let toml = r#"
name = "test"
description = "Empty version"
version = ""
"#;

        let result = PluginInfo::parse_str(toml, Path::new("test.toml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty 'version'"));
    }
}
