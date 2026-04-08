//! Parser for plugin `.info.toml` manifest files.
//!
//! Each plugin has a `{name}.info.toml` file that declares metadata:
//! - name, version, description
//! - dependencies (other plugins that must load first)
//! - taps (which tap functions the plugin implements)

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

    /// Plugin API version compatibility target (e.g., "0.2").
    #[serde(default = "default_api_version")]
    pub api_version: String,

    /// Whether this plugin should be auto-enabled on first install.
    #[serde(default = "default_true")]
    pub default_enabled: bool,

    /// Other plugins this one depends on (loaded first).
    #[serde(default)]
    pub dependencies: Vec<String>,

    /// Tap configuration.
    #[serde(default)]
    pub taps: TapConfig,

    /// Migration configuration.
    #[serde(default)]
    pub migrations: MigrationConfig,
}

/// Configuration for plugin-declared SQL migrations.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MigrationConfig {
    /// Ordered list of SQL migration files relative to the plugin directory.
    #[serde(default)]
    pub files: Vec<String>,

    /// Plugins whose migrations must run before this plugin's migrations.
    #[serde(default)]
    pub depends_on: Vec<String>,
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
    "tap_item_presave",
    "tap_item_access",
    "tap_field_access",
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
    "tap_user_logout",
    "tap_user_register",
    "tap_user_update",
    "tap_user_delete",
    "tap_user_export",
    // AI governance
    "tap_ai_request",
    "tap_chat_actions",
    // Security
    "tap_csp_alter",
    // Comments
    "tap_comment_insert",
    "tap_comment_update",
    "tap_comment_delete",
    "tap_comment_access",
    // Gather extensions
    "tap_gather_extend",
];

fn default_true() -> bool {
    true
}

fn default_api_version() -> String {
    "0.2".to_string()
}

impl PluginInfo {
    /// Parse a plugin info file from the given path.
    pub fn parse(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read plugin info file: {}", path.display()))?;

        Self::parse_str(&content, path)
    }

    /// Parse plugin info from a TOML string.
    pub fn parse_str(content: &str, path: &Path) -> Result<Self> {
        let info: PluginInfo = toml::from_str(content)
            .with_context(|| format!("failed to parse plugin info TOML at {}", path.display()))?;

        info.validate(path)?;
        Ok(info)
    }

    /// Validate the parsed plugin info.
    fn validate(&self, path: &Path) -> Result<()> {
        // Validate name is not empty
        if self.name.is_empty() {
            anyhow::bail!("plugin info at {} has empty 'name' field", path.display());
        }

        // Validate version is not empty
        if self.version.is_empty() {
            anyhow::bail!(
                "plugin '{}' at {} has empty 'version' field",
                self.name,
                path.display()
            );
        }

        // Validate api_version format (MAJOR.MINOR, both numeric)
        let api_parts: Vec<&str> = self.api_version.split('.').collect();
        if api_parts.len() != 2 || api_parts.iter().any(|p| p.parse::<u32>().is_err()) {
            anyhow::bail!(
                "plugin '{}' at {} has invalid 'api_version' field '{}' (expected MAJOR.MINOR, e.g., '0.2')",
                self.name,
                path.display(),
                self.api_version
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

        // Validate migration file paths: must be relative, no traversal, .sql only
        for file in &self.migrations.files {
            let p = Path::new(file);
            if p.is_absolute() {
                anyhow::bail!(
                    "plugin '{}': migration file '{}' must be a relative path",
                    self.name,
                    file
                );
            }
            if p.components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                anyhow::bail!(
                    "plugin '{}': migration file '{}' contains '..' path segment",
                    self.name,
                    file
                );
            }
            if !file.ends_with(".sql") {
                anyhow::bail!(
                    "plugin '{}': migration file '{}' must have .sql extension",
                    self.name,
                    file
                );
            }
        }

        Ok(())
    }

    /// Check if this plugin's declared API version is compatible with the kernel.
    ///
    /// Rule: plugin MAJOR == kernel MAJOR AND plugin MINOR <= kernel MINOR.
    /// A plugin built for API 0.1 works on kernel API 0.2.
    /// A plugin built for API 0.3 does NOT work on kernel API 0.2.
    pub fn check_api_compatibility(&self) -> Result<()> {
        use super::KERNEL_API_VERSION;

        let parts: Vec<u32> = self
            .api_version
            .split('.')
            .filter_map(|p| p.parse().ok())
            .collect();

        if parts.len() != 2 {
            anyhow::bail!(
                "plugin '{}' has invalid api_version '{}'",
                self.name,
                self.api_version
            );
        }

        let (plugin_major, plugin_minor) = (parts[0], parts[1]);
        let (kernel_major, kernel_minor) = KERNEL_API_VERSION;

        if plugin_major != kernel_major {
            anyhow::bail!(
                "plugin '{}' requires API {}.{} but kernel provides API {}.{}. \
                 Major version mismatch — plugin is incompatible with this kernel.",
                self.name,
                plugin_major,
                plugin_minor,
                kernel_major,
                kernel_minor
            );
        }

        if plugin_minor > kernel_minor {
            anyhow::bail!(
                "plugin '{}' requires API {}.{} but kernel provides API {}.{}. \
                 Plugin requires a newer kernel (API {}.{}+).",
                self.name,
                plugin_major,
                plugin_minor,
                kernel_major,
                kernel_minor,
                plugin_major,
                plugin_minor
            );
        }

        Ok(())
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_info() {
        let toml = r#"
name = "trovato_blog"
description = "Provides a blog content type"
version = "1.0.0"
dependencies = ["item", "categories"]

[taps]
implements = ["tap_item_info", "tap_item_view", "tap_menu", "tap_perm"]
weight = 0
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert_eq!(info.name, "trovato_blog");
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

    #[test]
    fn parse_migration_config() {
        let toml = r#"
name = "netgrasp"
description = "Network monitoring"
version = "1.0.0"

[migrations]
files = ["migrations/001_create_devices.sql", "migrations/002_create_events.sql"]
depends_on = ["trovato_blog"]
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert_eq!(info.migrations.files.len(), 2);
        assert_eq!(
            info.migrations.files[0],
            "migrations/001_create_devices.sql"
        );
        assert_eq!(info.migrations.depends_on, vec!["trovato_blog"]);
    }

    #[test]
    fn parse_no_migrations_defaults_empty() {
        let toml = r#"
name = "simple"
description = "No migrations"
version = "1.0.0"
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert!(info.migrations.files.is_empty());
        assert!(info.migrations.depends_on.is_empty());
    }

    #[test]
    fn reject_migration_path_traversal() {
        let toml = r#"
name = "evil"
description = "Path traversal"
version = "1.0.0"

[migrations]
files = ["../../../etc/passwd.sql"]
"#;

        let result = PluginInfo::parse_str(toml, Path::new("test.toml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains(".."));
    }

    #[test]
    fn reject_migration_absolute_path() {
        let toml = r#"
name = "evil"
description = "Absolute path"
version = "1.0.0"

[migrations]
files = ["/tmp/malicious.sql"]
"#;

        let result = PluginInfo::parse_str(toml, Path::new("test.toml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("relative path"));
    }

    #[test]
    fn reject_migration_non_sql() {
        let toml = r#"
name = "bad"
description = "Non-SQL migration"
version = "1.0.0"

[migrations]
files = ["migrations/001_create.txt"]
"#;

        let result = PluginInfo::parse_str(toml, Path::new("test.toml"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains(".sql"));
    }

    #[test]
    fn parse_default_enabled_false() {
        let toml = r#"
name = "argus"
description = "News intelligence"
version = "1.0.0"
default_enabled = false
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert!(!info.default_enabled);
    }

    #[test]
    fn default_enabled_is_true_when_omitted() {
        let toml = r#"
name = "trovato_blog"
description = "Blog plugin"
version = "1.0.0"
"#;

        let info = PluginInfo::parse_str(toml, Path::new("test.toml")).unwrap();
        assert!(info.default_enabled);
    }

    #[test]
    fn default_api_version_is_0_2() {
        let toml = r#"
name = "test_plugin"
description = "test"
version = "1.0.0"
"#;
        let info: PluginInfo = toml::from_str(toml).unwrap();
        assert_eq!(info.api_version, "0.2");
    }

    #[test]
    fn explicit_api_version_parses() {
        let toml = r#"
name = "test_plugin"
description = "test"
version = "1.0.0"
api_version = "1.0"
"#;
        let info: PluginInfo = toml::from_str(toml).unwrap();
        assert_eq!(info.api_version, "1.0");
    }

    #[test]
    fn invalid_api_version_rejected() {
        let toml = r#"
name = "test_plugin"
description = "test"
version = "1.0.0"
api_version = "abc"
"#;
        let info: PluginInfo = toml::from_str(toml).unwrap();
        let result = info.validate(std::path::Path::new("/test"));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid 'api_version'")
        );
    }

    #[test]
    fn three_part_api_version_rejected() {
        let toml = r#"
name = "test_plugin"
description = "test"
version = "1.0.0"
api_version = "1.2.3"
"#;
        let info: PluginInfo = toml::from_str(toml).unwrap();
        let result = info.validate(std::path::Path::new("/test"));
        assert!(result.is_err());
    }

    #[test]
    fn api_compat_same_version_ok() {
        let info = make_info("0.2");
        assert!(info.check_api_compatibility().is_ok());
    }

    #[test]
    fn api_compat_older_minor_ok() {
        let info = make_info("0.1");
        assert!(info.check_api_compatibility().is_ok());
    }

    #[test]
    fn api_compat_newer_minor_rejected() {
        let info = make_info("0.3");
        let err = info.check_api_compatibility().unwrap_err();
        assert!(err.to_string().contains("requires a newer kernel"));
    }

    #[test]
    fn api_compat_major_mismatch_rejected() {
        let info = make_info("1.0");
        let err = info.check_api_compatibility().unwrap_err();
        assert!(err.to_string().contains("Major version mismatch"));
    }

    fn make_info(api_version: &str) -> PluginInfo {
        PluginInfo {
            name: "test_plugin".to_string(),
            description: "test".to_string(),
            version: "1.0.0".to_string(),
            api_version: api_version.to_string(),
            default_enabled: true,
            dependencies: vec![],
            taps: super::TapConfig::default(),
            migrations: super::MigrationConfig::default(),
        }
    }
}
