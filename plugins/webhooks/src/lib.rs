//! Webhooks plugin for Trovato.
//!
//! Provides event-driven webhook dispatch with HMAC-SHA256 signatures
//! and exponential backoff retry.

use trovato_sdk::prelude::*;

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        "administer webhooks",
        "Administer webhooks",
    )]
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/config/webhooks", "Webhooks")
            .callback("webhook_admin")
            .permission("administer webhooks")
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
        assert_eq!(perms[0].name, "administer webhooks");
    }

    #[test]
    fn menu_returns_one_route() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].path, "/admin/config/webhooks");
    }
}
