//! OAuth2 provider plugin for Trovato.
//!
//! Provides OAuth2 authorization server with JWT token issuance,
//! supporting authorization_code, client_credentials, and refresh_token grants.

use trovato_sdk::prelude::*;

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        "administer oauth clients",
        "Administer OAuth clients",
    )]
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/config/oauth", "OAuth Clients")
            .callback("oauth_admin")
            .permission("administer oauth clients")
            .parent("/admin/config"),
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn perm_returns_one_permission() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0].name, "administer oauth clients");
    }

    #[test]
    fn menu_returns_one_route() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].path, "/admin/config/oauth");
    }
}
