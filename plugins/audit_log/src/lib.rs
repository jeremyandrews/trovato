//! Audit log plugin for Trovato.
//!
//! Provides audit logging for content, auth, and permission changes.

use trovato_sdk::prelude::*;

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        "view audit log",
        "View audit log",
    )]
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/reports/audit", "Audit Log")
            .callback("audit_log_admin")
            .permission("view audit log")
            .parent("/admin/reports"),
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
        assert_eq!(perms[0].name, "view audit log");
    }

    #[test]
    fn menu_returns_one_route() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].path, "/admin/reports/audit");
    }
}
