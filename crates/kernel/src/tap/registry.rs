//! Tap registry - indexes which plugins implement which taps.
//!
//! The registry maps tap names to an ordered list of plugins that implement them.
//! Plugins are sorted by weight (lower = higher priority, called first).

use std::collections::HashMap;
use std::sync::Arc;

use crate::plugin::{CompiledPlugin, PluginRuntime};

/// A registered tap handler with plugin reference and priority.
#[derive(Debug, Clone)]
pub struct TapHandler {
    /// The plugin that implements this tap.
    pub plugin: Arc<CompiledPlugin>,
    /// Weight for ordering (lower = higher priority).
    pub weight: i32,
}

/// Registry mapping tap names to ordered handlers.
///
/// When a tap is invoked, handlers are called in weight order.
/// Multiple plugins can implement the same tap.
#[derive(Debug)]
pub struct TapRegistry {
    /// Map from tap name to ordered list of handlers.
    handlers: HashMap<String, Vec<TapHandler>>,
}

impl TapRegistry {
    /// Build a tap registry from loaded plugins.
    ///
    /// Scans all plugins for their implemented taps and indexes them.
    /// Handlers are sorted by weight (lower = higher priority).
    pub fn from_plugins(runtime: &PluginRuntime) -> Self {
        let mut handlers: HashMap<String, Vec<TapHandler>> = HashMap::new();

        for plugin in runtime.plugins().values() {
            let weight = plugin.info.taps.weight;

            for tap_name in &plugin.info.taps.implements {
                let handler = TapHandler {
                    plugin: Arc::clone(plugin),
                    weight,
                };

                handlers.entry(tap_name.clone()).or_default().push(handler);
            }
        }

        // Sort each tap's handlers by weight
        for handlers_list in handlers.values_mut() {
            handlers_list.sort_by_key(|h| h.weight);
        }

        Self { handlers }
    }

    /// Get handlers for a tap, in weight order.
    ///
    /// Returns an empty slice if no plugins implement the tap.
    pub fn get_handlers(&self, tap_name: &str) -> &[TapHandler] {
        self.handlers
            .get(tap_name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Check if any plugin implements a tap.
    pub fn has_tap(&self, tap_name: &str) -> bool {
        self.handlers
            .get(tap_name)
            .is_some_and(|handlers| !handlers.is_empty())
    }

    /// Get all registered tap names.
    pub fn tap_names(&self) -> impl Iterator<Item = &str> {
        self.handlers.keys().map(|s| s.as_str())
    }

    /// Get the count of handlers for a tap.
    pub fn handler_count(&self, tap_name: &str) -> usize {
        self.handlers.get(tap_name).map(|v| v.len()).unwrap_or(0)
    }

    /// Get total number of registered taps.
    pub fn tap_count(&self) -> usize {
        self.handlers.len()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::plugin::PluginConfig;
    use std::path::Path;

    fn test_plugins_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("plugins")
    }

    #[test]
    fn registry_from_empty_runtime() {
        let runtime = PluginRuntime::new(&PluginConfig::default()).unwrap();
        let registry = TapRegistry::from_plugins(&runtime);

        assert_eq!(registry.tap_count(), 0);
        assert!(!registry.has_tap("tap_item_view"));
        assert!(registry.get_handlers("tap_item_view").is_empty());
    }

    #[test]
    fn registry_indexes_plugin_taps() {
        let mut runtime = PluginRuntime::new(&PluginConfig::default()).unwrap();
        let blog_dir = test_plugins_dir().join("blog");
        runtime.load_plugin(&blog_dir).expect("failed to load blog");

        let registry = TapRegistry::from_plugins(&runtime);

        // Blog plugin declares these taps
        assert!(registry.has_tap("tap_item_info"));
        assert!(registry.has_tap("tap_item_view"));
        assert!(registry.has_tap("tap_item_access"));
        assert!(registry.has_tap("tap_menu"));
        assert!(registry.has_tap("tap_perm"));

        // Each tap should have exactly one handler (from blog)
        assert_eq!(registry.handler_count("tap_item_view"), 1);

        // Handler should reference the blog plugin
        let handlers = registry.get_handlers("tap_item_view");
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].plugin.info.name, "blog");
        assert_eq!(handlers[0].weight, 0);
    }

    #[test]
    fn registry_unknown_tap_returns_empty() {
        let runtime = PluginRuntime::new(&PluginConfig::default()).unwrap();
        let registry = TapRegistry::from_plugins(&runtime);

        assert!(!registry.has_tap("nonexistent_tap"));
        assert!(registry.get_handlers("nonexistent_tap").is_empty());
        assert_eq!(registry.handler_count("nonexistent_tap"), 0);
    }

    #[test]
    fn registry_lists_tap_names() {
        let mut runtime = PluginRuntime::new(&PluginConfig::default()).unwrap();
        let blog_dir = test_plugins_dir().join("blog");
        runtime.load_plugin(&blog_dir).expect("failed to load blog");

        let registry = TapRegistry::from_plugins(&runtime);
        let names: Vec<_> = registry.tap_names().collect();

        assert!(names.contains(&"tap_item_info"));
        assert!(names.contains(&"tap_item_view"));
        assert_eq!(names.len(), 5); // blog implements 5 taps
    }
}
