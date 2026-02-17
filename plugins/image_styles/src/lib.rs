//! Image styles plugin for Trovato.
//!
//! Provides on-demand image derivative generation with configurable
//! effect chains (scale, crop, resize, desaturate).

use trovato_sdk::prelude::*;

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        "administer image styles",
        "Administer image styles",
    )]
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/config/image-styles", "Image Styles")
            .callback("image_style_admin")
            .permission("administer image styles")
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
        assert_eq!(perms[0].name, "administer image styles");
    }

    #[test]
    fn menu_returns_one_route() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].path, "/admin/config/image-styles");
    }
}
