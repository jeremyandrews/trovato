//! Redirects plugin for Trovato.
//!
//! Provides URL redirect management with source → destination mapping.

use trovato_sdk::prelude::*;

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new("administer redirects", "Administer redirects"),
        PermissionDefinition::new("view redirects", "View redirects"),
    ]
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/config/redirects", "Redirects")
            .callback("redirect_admin")
            .permission("administer redirects")
            .parent("/admin/config"),
    ]
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn perm_returns_two_permissions() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 2);
    }

    #[test]
    fn menu_returns_one_route() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].path, "/admin/config/redirects");
    }
}
