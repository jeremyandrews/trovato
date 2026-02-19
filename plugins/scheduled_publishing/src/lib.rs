//! Scheduled publishing plugin for Trovato.
//!
//! Allows scheduling items for future publish/unpublish via
//! field_publish_on and field_unpublish_on JSONB fields.

use trovato_sdk::prelude::*;

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        "schedule publishing",
        "Schedule content publishing",
    )]
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/admin/content/scheduled", "Scheduled")
            .callback("scheduled_items")
            .permission("schedule publishing")
            .parent("/admin/content"),
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
    }

    #[test]
    fn menu_returns_one_route() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].path, "/admin/content/scheduled");
    }
}
