//! Locale plugin for Trovato.
//!
//! Provides interface string translation with .po file import support.

use trovato_sdk::prelude::*;

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new("administer locale", "Administer locale settings"),
        PermissionDefinition::new("translate interface", "Translate interface strings"),
    ]
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/config/locale", "Locale")
            .callback("locale_admin")
            .permission("administer locale")
            .parent("/admin/config"),
        MenuDefinition::new("/admin/config/locale/import", "Import Translations")
            .callback("locale_import")
            .permission("translate interface")
            .parent("/admin/config/locale"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perm_returns_two_permissions() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 2);
    }

    #[test]
    fn menu_returns_two_routes() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 2);
        assert_eq!(menus[0].path, "/admin/config/locale");
    }
}
