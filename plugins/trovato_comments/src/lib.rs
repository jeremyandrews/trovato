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
        PermissionDefinition::new("skip comment approval", "Skip comment approval"),
    ]
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/content/comments", "Comments")
            .callback("comment_admin")
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
    fn perm_returns_four_permissions() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 4);
    }

    #[test]
    fn menu_returns_one_route() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].path, "/admin/content/comments");
    }
}
