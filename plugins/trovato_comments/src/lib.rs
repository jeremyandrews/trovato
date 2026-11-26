//! Comments plugin for Trovato.
//!
//! Provides the declarative layer for the comment system.
//! Runtime code (routes, model) remains in the kernel.

use trovato_sdk::prelude::*;

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new("administer comments", "Administer comments"),
        PermissionDefinition::new("post comments", "Post comments"),
        PermissionDefinition::new("edit own comments", "Edit own comments"),
        PermissionDefinition::new("delete own comments", "Delete own comments"),
        PermissionDefinition::new("skip comment approval", "Skip comment approval"),
        // An item's own override of its content type's comment setting
        // (B239): open, closed or hidden on this item alone. The type's own
        // setting is behind `administer site`, which does not fit an editor
        // who should only ever touch one item's comments.
        PermissionDefinition::new("set item comment status", "Set an item's comment status"),
    ]
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/content/comments", "Comments")
            .permission("administer comments")
            .parent("/admin/content"),
    ]
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn perm_returns_six_permissions() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 6);
        assert!(perms.iter().any(|p| p.name == "set item comment status"));
    }

    #[test]
    fn menu_returns_one_route() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].path, "/admin/content/comments");
    }
}
