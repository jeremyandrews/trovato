//! Menu registry - collects and manages menu definitions from plugins.
//!
//! Plugins register menus via the `tap_menu` tap, which returns JSON arrays
//! of MenuDefinition objects.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// A menu/route definition from a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MenuDefinition {
    /// URL path pattern (e.g., "/admin/content", "/blog/:slug")
    pub path: String,
    /// Human-readable title
    pub title: String,
    /// Plugin that owns this menu
    #[serde(default)]
    pub plugin: String,
    /// Required permission to access (empty = public)
    #[serde(default)]
    pub permission: String,
    /// Parent menu path for hierarchy
    #[serde(default)]
    pub parent: Option<String>,
    /// Sort weight (lower = higher priority)
    #[serde(default)]
    pub weight: i32,
    /// Whether this appears in navigation
    #[serde(default = "default_true")]
    pub visible: bool,
    /// HTTP method (GET, POST, etc.)
    #[serde(default = "default_get")]
    pub method: String,
    /// Handler type: "page", "api", "form"
    #[serde(default = "default_page")]
    pub handler_type: String,
}

fn default_true() -> bool {
    true
}
fn default_get() -> String {
    "GET".to_string()
}
fn default_page() -> String {
    "page".to_string()
}

/// Result of matching a path against registered routes.
#[derive(Debug, Clone)]
pub struct RouteMatch {
    /// The matched menu definition.
    pub menu: MenuDefinition,
    /// Path parameters extracted (e.g., {"slug": "my-post"})
    pub params: HashMap<String, String>,
}

/// Registry of all menu definitions from plugins.
#[derive(Debug)]
pub struct MenuRegistry {
    /// All menu definitions, indexed by path
    menus: HashMap<String, MenuDefinition>,
    /// Menus organized by parent for tree building
    children: HashMap<String, Vec<String>>,
    /// Route patterns for matching (path -> menu path)
    routes: Vec<(String, String)>,
}

impl MenuRegistry {
    /// Create an empty menu registry.
    pub fn new() -> Self {
        Self {
            menus: HashMap::new(),
            children: HashMap::new(),
            routes: Vec::new(),
        }
    }

    /// Create a menu registry from JSON arrays returned by tap_menu.
    ///
    /// Each element in `menu_jsons` is a (plugin_name, json_array) tuple.
    pub fn from_tap_results(menu_jsons: Vec<(String, String)>) -> Self {
        let mut registry = Self::new();

        for (plugin_name, json) in menu_jsons {
            match serde_json::from_str::<Vec<MenuDefinition>>(&json) {
                Ok(menus) => {
                    for mut menu in menus {
                        menu.plugin = plugin_name.clone();
                        registry.register(menu);
                    }
                }
                Err(e) => {
                    warn!(
                        plugin = %plugin_name,
                        error = %e,
                        "failed to parse tap_menu result"
                    );
                }
            }
        }

        registry.build_routes();
        registry
    }

    /// Register a menu definition.
    pub fn register(&mut self, menu: MenuDefinition) {
        let path = menu.path.clone();

        // Track parent-child relationships
        if let Some(ref parent) = menu.parent {
            self.children
                .entry(parent.clone())
                .or_default()
                .push(path.clone());
        }

        self.menus.insert(path, menu);
    }

    /// Build route patterns for path matching.
    fn build_routes(&mut self) {
        self.routes = self
            .menus
            .keys()
            .map(|path| {
                // Convert path params to regex-like pattern for sorting
                // More specific routes (fewer params) come first
                let _specificity = path.matches(':').count();
                (path.clone(), path.clone())
            })
            .collect();

        // Sort by specificity (fewer params = more specific = first)
        self.routes.sort_by_key(|(path, _)| {
            let param_count = path.matches(':').count();
            let segment_count = path.matches('/').count();
            (param_count, -(segment_count as i32))
        });

        debug!(routes = self.routes.len(), "built route table");
    }

    /// Match a request path against registered routes.
    pub fn match_path(&self, path: &str) -> Option<RouteMatch> {
        for (pattern, menu_path) in &self.routes {
            if let Some(params) = match_pattern(pattern, path)
                && let Some(menu) = self.menus.get(menu_path)
            {
                return Some(RouteMatch {
                    menu: menu.clone(),
                    params,
                });
            }
        }
        None
    }

    /// Get a menu by its path.
    pub fn get(&self, path: &str) -> Option<&MenuDefinition> {
        self.menus.get(path)
    }

    /// Get all menus.
    pub fn all(&self) -> impl Iterator<Item = &MenuDefinition> {
        self.menus.values()
    }

    /// Get child menus of a parent path.
    pub fn children_of(&self, parent: &str) -> Vec<&MenuDefinition> {
        self.children
            .get(parent)
            .map(|paths| paths.iter().filter_map(|p| self.menus.get(p)).collect())
            .unwrap_or_default()
    }

    /// Get top-level menus (no parent).
    pub fn root_menus(&self) -> Vec<&MenuDefinition> {
        self.menus
            .values()
            .filter(|m| m.parent.is_none() && m.visible)
            .collect()
    }

    /// Get menu count.
    pub fn len(&self) -> usize {
        self.menus.len()
    }

    /// Check if registry is empty.
    pub fn is_empty(&self) -> bool {
        self.menus.is_empty()
    }
}

impl Default for MenuRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Match a route pattern against a path, extracting parameters.
///
/// Pattern: "/blog/:slug/edit"
/// Path: "/blog/my-post/edit"
/// Result: Some({"slug": "my-post"})
fn match_pattern(pattern: &str, path: &str) -> Option<HashMap<String, String>> {
    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();

    if pattern_parts.len() != path_parts.len() {
        return None;
    }

    let mut params = HashMap::new();

    for (pat, actual) in pattern_parts.iter().zip(path_parts.iter()) {
        if let Some(param_name) = pat.strip_prefix(':') {
            // Parameter segment
            params.insert(param_name.to_string(), actual.to_string());
        } else if pat != actual {
            // Literal segment doesn't match
            return None;
        }
    }

    Some(params)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn match_pattern_exact() {
        let params = match_pattern("/admin/content", "/admin/content");
        assert!(params.is_some());
        assert!(params.unwrap().is_empty());
    }

    #[test]
    fn match_pattern_with_param() {
        let params = match_pattern("/blog/:slug", "/blog/my-post");
        assert!(params.is_some());
        let params = params.unwrap();
        assert_eq!(params.get("slug"), Some(&"my-post".to_string()));
    }

    #[test]
    fn match_pattern_multiple_params() {
        let params = match_pattern("/api/:type/:id", "/api/posts/123");
        assert!(params.is_some());
        let params = params.unwrap();
        assert_eq!(params.get("type"), Some(&"posts".to_string()));
        assert_eq!(params.get("id"), Some(&"123".to_string()));
    }

    #[test]
    fn match_pattern_no_match() {
        assert!(match_pattern("/admin/content", "/admin/users").is_none());
        assert!(match_pattern("/blog/:slug", "/blog/a/b").is_none());
    }

    #[test]
    fn registry_from_json() {
        let json = r#"[
            {"path": "/admin/blog", "title": "Blog"},
            {"path": "/admin/blog/:id", "title": "Edit Post"}
        ]"#;

        let registry = MenuRegistry::from_tap_results(vec![("blog".to_string(), json.to_string())]);

        assert_eq!(registry.len(), 2);
        assert!(registry.get("/admin/blog").is_some());
    }

    #[test]
    fn registry_match_path() {
        let json = r#"[
            {"path": "/blog", "title": "Blog"},
            {"path": "/blog/:slug", "title": "Post"}
        ]"#;

        let registry = MenuRegistry::from_tap_results(vec![("blog".to_string(), json.to_string())]);

        let result = registry.match_path("/blog/hello-world");
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.menu.path, "/blog/:slug");
        assert_eq!(result.params.get("slug"), Some(&"hello-world".to_string()));
    }

    #[test]
    fn registry_parent_child() {
        let json = r#"[
            {"path": "/admin", "title": "Admin"},
            {"path": "/admin/content", "title": "Content", "parent": "/admin"},
            {"path": "/admin/users", "title": "Users", "parent": "/admin"}
        ]"#;

        let registry =
            MenuRegistry::from_tap_results(vec![("admin".to_string(), json.to_string())]);

        let children = registry.children_of("/admin");
        assert_eq!(children.len(), 2);
    }
}
