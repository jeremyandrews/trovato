//! Plugin route gating.
//!
//! Determines which plugins should be auto-enabled on first install and which
//! kernel routes should be conditionally registered based on plugin status.

/// Compute whether a newly-discovered plugin should be auto-enabled.
///
/// A plugin is enabled on first install only if:
/// 1. Its `default_enabled` field is `true` (from info.toml), AND
/// 2. Its name is NOT listed in the `DISABLED_PLUGINS` env var.
///
/// Existing installs are never affected (ON CONFLICT DO NOTHING in SQL).
pub fn should_auto_enable(
    default_enabled: bool,
    disabled_plugins: &[String],
    plugin_name: &str,
) -> bool {
    default_enabled && !disabled_plugins.iter().any(|d| d == plugin_name)
}

/// Plugins whose kernel routes are runtime-gated.
///
/// When a plugin in this list is disabled, its HTTP routes return 404 via
/// runtime middleware. Each entry names the plugin and describes the routes
/// it gates.
///
/// **Maintenance rule:** any new route module that belongs to a plugin must be
/// added here **and** in [`crate::routes::gated_plugin_routes`], which is the
/// single source of truth for route registration.
pub const GATED_ROUTE_PLUGINS: &[GatedPlugin] = &[
    GatedPlugin {
        name: "categories",
        description: "Category and tag admin UI + API routes",
    },
    GatedPlugin {
        name: "comments",
        description: "Comment moderation admin UI + API routes",
    },
    GatedPlugin {
        name: "content_locking",
        description: "Content lock API routes",
    },
    GatedPlugin {
        name: "image_styles",
        description: "Image style derivative routes",
    },
    GatedPlugin {
        name: "oauth2",
        description: "OAuth2 authorization routes",
    },
];

/// A plugin whose kernel routes are runtime-gated.
pub struct GatedPlugin {
    /// Plugin machine name (must match the `name` field in info.toml).
    pub name: &'static str,
    /// Human-readable description of what routes this gates.
    pub description: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::Path;

    // -------------------------------------------------------------------------
    // should_auto_enable
    // -------------------------------------------------------------------------

    #[test]
    fn auto_enable_default_true_not_disabled() {
        let disabled: Vec<String> = Vec::new();
        assert!(should_auto_enable(true, &disabled, "blog"));
    }

    #[test]
    fn auto_enable_default_false() {
        let disabled: Vec<String> = Vec::new();
        assert!(!should_auto_enable(false, &disabled, "argus"));
    }

    #[test]
    fn auto_enable_default_true_but_disabled() {
        let disabled: Vec<String> = vec!["blog".into(), "media".into()];
        assert!(!should_auto_enable(true, &disabled, "blog"));
    }

    #[test]
    fn auto_enable_default_false_and_disabled() {
        let disabled: Vec<String> = vec!["argus".into()];
        assert!(!should_auto_enable(false, &disabled, "argus"));
    }

    #[test]
    fn auto_enable_disabled_set_does_not_affect_other_plugins() {
        let disabled: Vec<String> = vec!["blog".into()];
        assert!(should_auto_enable(true, &disabled, "categories"));
    }

    #[test]
    fn auto_enable_empty_disabled_set() {
        let disabled: Vec<String> = Vec::new();
        assert!(should_auto_enable(true, &disabled, "anything"));
        assert!(!should_auto_enable(false, &disabled, "anything"));
    }

    // -------------------------------------------------------------------------
    // GATED_ROUTE_PLUGINS validity
    // -------------------------------------------------------------------------

    #[test]
    fn gated_plugins_exist_on_disk() {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        let project_root = Path::new(&manifest_dir)
            .parent() // crates/
            .and_then(|p| p.parent()) // project root
            .unwrap_or(Path::new("."));
        let plugins_dir = project_root.join("plugins");

        assert!(
            plugins_dir.exists(),
            "plugins/ directory not found at {}",
            plugins_dir.display()
        );

        let discovered = crate::plugin::PluginRuntime::discover_plugins(&plugins_dir);
        for gated in GATED_ROUTE_PLUGINS {
            assert!(
                discovered.contains_key(gated.name),
                "GATED_ROUTE_PLUGINS references '{}' which was not found in {}. \
                 Either the plugin is missing or the gate entry is stale.",
                gated.name,
                plugins_dir.display(),
            );
        }
    }

    #[test]
    fn gated_plugin_names_are_unique() {
        let mut seen = HashSet::new();
        for gated in GATED_ROUTE_PLUGINS {
            assert!(
                seen.insert(gated.name),
                "duplicate entry '{}' in GATED_ROUTE_PLUGINS",
                gated.name,
            );
        }
    }

    /// Ensures the documentation constant `GATED_ROUTE_PLUGINS` stays in sync
    /// with the actual set of runtime-gated plugin names in `routes/mod.rs`.
    #[test]
    fn gated_route_plugins_matches_runtime_gates() {
        let doc_names: HashSet<&str> = GATED_ROUTE_PLUGINS.iter().map(|g| g.name).collect();
        let runtime_names: HashSet<&str> =
            crate::routes::RUNTIME_GATED_NAMES.iter().copied().collect();
        assert_eq!(
            doc_names, runtime_names,
            "GATED_ROUTE_PLUGINS and RUNTIME_GATED_NAMES are out of sync"
        );
    }
}
