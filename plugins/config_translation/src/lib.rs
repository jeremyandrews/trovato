//! Config translation plugin for Trovato.
//!
//! Provides configuration entity translation with language overlay.

use trovato_sdk::prelude::*;

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        "translate configuration",
        "Translate configuration",
    )]
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/config/translate", "Config Translation")
            .callback("config_translate_admin")
            .permission("translate configuration")
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
        assert_eq!(perms[0].name, "translate configuration");
    }

    #[test]
    fn menu_returns_one_route() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].path, "/admin/config/translate");
    }
}
