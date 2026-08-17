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
    /// Handler name dispatched to the owning plugin's `tap_api` when
    /// `handler_type` is `"api"` (**G-NO-PLUGIN-HTTP**, K1 fix 1).
    ///
    /// The SDK's `MenuDefinition` has carried this field with a builder since
    /// before the freeze; the kernel's did not, so it was **dropped on
    /// deserialize** and a plugin author who set it had registered nothing. The
    /// registry now keeps it, and `routes::plugin_api` dispatches on it.
    #[serde(default)]
    pub callback: String,
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
    /// Whether this is a local task (tab-style navigation on entity pages)
    #[serde(default)]
    pub local_task: bool,
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

    /// Root navigation entries this viewer may access, sorted by weight.
    ///
    /// The `permission` field on a [`MenuDefinition`] gates visibility **per
    /// viewer**: an entry appears when it requires no permission, when the
    /// viewer holds the one it requires, or when the viewer is an admin. Admins
    /// implicitly hold every permission, matching
    /// [`require_permission`](crate::routes::helpers::require_permission), so
    /// navigation never hides a page an admin can in fact open.
    ///
    /// Navigation used to be built as `root_menus().filter(|m|
    /// m.permission.is_empty())`, which reads as a permission check and is not
    /// one: it kept only the entries that need no permission, so **every**
    /// gated entry was hidden from **every** viewer, the holder and the admin
    /// included. A plugin declaring a gated nav entry could not get it into the
    /// navigation at all — Argus's `/stories` and `/articles`, both gated on
    /// `access content`, were unreachable from the menu for everyone.
    pub fn root_menus_for(&self, viewer: &crate::tap::UserContext) -> Vec<MenuDefinition> {
        let mut menus: Vec<MenuDefinition> = self
            .root_menus()
            .into_iter()
            .filter(|m| viewer_may_see(m, viewer))
            .cloned()
            .collect();
        menus.sort_by_key(|m| m.weight);
        menus
    }

    /// Get visible local task menus for a parent path, sorted by weight.
    ///
    /// Only returns menus where `visible` is `true`. Menus with
    /// `visible: false` are excluded even if `local_task` is set — as of
    /// the Epic 28 audit no registered plugin uses that combination.
    ///
    /// Permission filtering is handled at the route level (admin routes
    /// require `require_admin()`), so tabs are only shown on pages the
    /// user already has access to.
    pub fn local_tasks(&self, parent: &str) -> Vec<&MenuDefinition> {
        let mut tasks: Vec<&MenuDefinition> = self
            .menus
            .values()
            .filter(|m| m.local_task && m.visible && m.parent.as_deref() == Some(parent))
            .collect();
        tasks.sort_by_key(|m| m.weight);
        tasks
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

/// Whether a viewer may see a menu entry, by the entry's declared permission.
///
/// Empty permission means public. Admins hold every permission implicitly.
fn viewer_may_see(menu: &MenuDefinition, viewer: &crate::tap::UserContext) -> bool {
    menu.permission.is_empty() || viewer.is_admin() || viewer.has_permission(&menu.permission)
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
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use uuid::Uuid;

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
    fn registry_local_tasks() {
        let json = r#"[
            {"path": "/admin/content/:id", "title": "View", "parent": "/admin/content/:id", "local_task": true, "weight": 0},
            {"path": "/admin/content/:id/edit", "title": "Edit", "parent": "/admin/content/:id", "local_task": true, "weight": 1},
            {"path": "/admin/content/:id/revisions", "title": "Revisions", "parent": "/admin/content/:id", "local_task": true, "weight": 2},
            {"path": "/admin/content", "title": "Content"}
        ]"#;

        let registry =
            MenuRegistry::from_tap_results(vec![("content".to_string(), json.to_string())]);

        let tasks = registry.local_tasks("/admin/content/:id");
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].title, "View");
        assert_eq!(tasks[1].title, "Edit");
        assert_eq!(tasks[2].title, "Revisions");

        // Non-local-task items should not appear
        let no_tasks = registry.local_tasks("/admin/content");
        assert!(no_tasks.is_empty());
    }

    // --- root_menus_for: per-viewer navigation visibility ---

    /// A public entry, a gated one, an admin-gated one, a child, and an
    /// invisible API route — the five shapes navigation has to sort out.
    fn nav_registry() -> MenuRegistry {
        let json = r#"[
            {"path": "/", "title": "Home", "weight": -10},
            {"path": "/stories", "title": "Stories", "permission": "access content", "weight": 0},
            {"path": "/admin/argus/feeds", "title": "Argus feeds", "permission": "argus:administer", "weight": 5},
            {"path": "/stories/:id", "title": "Story", "parent": "/stories"},
            {"path": "/argus/story/:id/react", "title": "React", "permission": "argus:react", "visible": false, "handler_type": "api", "method": "POST"}
        ]"#;
        MenuRegistry::from_tap_results(vec![("argus".to_string(), json.to_string())])
    }

    fn viewer(permissions: &[&str]) -> crate::tap::UserContext {
        crate::tap::UserContext::authenticated(
            Uuid::now_v7(),
            permissions.iter().map(|p| (*p).to_string()).collect(),
        )
    }

    fn titles(menus: &[MenuDefinition]) -> Vec<&str> {
        menus.iter().map(|m| m.title.as_str()).collect()
    }

    #[test]
    fn gated_menu_appears_for_a_viewer_holding_the_permission() {
        let menus = nav_registry().root_menus_for(&viewer(&["access content"]));
        assert_eq!(titles(&menus), vec!["Home", "Stories"]);
    }

    #[test]
    fn gated_menu_is_absent_for_a_viewer_without_the_permission() {
        let menus = nav_registry().root_menus_for(&viewer(&[]));
        assert_eq!(titles(&menus), vec!["Home"]);
    }

    #[test]
    fn gated_menu_appears_for_an_admin() {
        // Admins implicitly hold every permission, so navigation shows both
        // gated entries even though the admin's roles grant neither by name.
        let menus = nav_registry().root_menus_for(&viewer(&["administer site"]));
        assert_eq!(titles(&menus), vec!["Home", "Stories", "Argus feeds"]);
    }

    #[test]
    fn anonymous_viewer_with_the_permission_still_sees_the_gated_menu() {
        // The anonymous role commonly grants "access content", and an anonymous
        // context is not `authenticated` — visibility must key on the
        // permission, not on being logged in.
        let mut anon = crate::tap::UserContext::anonymous();
        anon.permissions = vec!["access content".to_string()];
        let menus = nav_registry().root_menus_for(&anon);
        assert_eq!(titles(&menus), vec!["Home", "Stories"]);
    }

    #[test]
    fn navigation_excludes_children_and_invisible_api_routes() {
        // An admin sees everything permission can allow, so anything still
        // missing here is excluded structurally: child entries belong under
        // their parent, and an `api` route is a write endpoint, not a link.
        let menus = nav_registry().root_menus_for(&viewer(&["administer site"]));
        assert!(!titles(&menus).contains(&"Story"));
        assert!(!titles(&menus).contains(&"React"));
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
