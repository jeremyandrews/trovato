//! Categories plugin for Trovato.
//!
//! Provides the declarative layer for category vocabulary and term management.
//! Runtime code (CategoryService, routes) remains in the kernel.

use trovato_sdk::prelude::*;

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new("administer categories", "Administer categories"),
        PermissionDefinition::new("create category terms", "Create category terms"),
        PermissionDefinition::new("edit category terms", "Edit category terms"),
        PermissionDefinition::new("delete category terms", "Delete category terms"),
    ]
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/categories", "Categories")
            .callback("category_admin")
            .permission("administer categories")
            .parent("/admin"),
        MenuDefinition::new("/admin/categories/:id/terms", "Terms")
            .callback("category_term_admin")
            .permission("administer categories"),
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
    fn menu_returns_two_routes() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 2);
        assert_eq!(menus[0].path, "/admin/categories");
    }
}
