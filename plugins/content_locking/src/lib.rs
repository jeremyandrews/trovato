//! Content locking plugin for Trovato.
//!
//! Provides pessimistic locking to prevent concurrent editing.

use trovato_sdk::prelude::*;

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        "break content lock",
        "Break content locks held by other users",
    )]
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/config/content-locking", "Content Locking")
            .callback("content_locking_admin")
            .permission("break content lock")
            .parent("/admin/config"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perm_returns_one_permission() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0].name, "break content lock");
    }

    #[test]
    fn menu_returns_one_route() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].path, "/admin/config/content-locking");
    }
}
