//! Subscription and notification plugin for Ritrovo conferences.
//!
//! Provides:
//! - Menu entry for user subscription management
//! - Subscribe/Unsubscribe toggle on conference pages (via tap_item_view)
//! - Queue declaration for notification delivery
//! - Permissions for notification features

use trovato_sdk::prelude::*;

/// Register notification-related permissions.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new(
            "manage own subscriptions",
            "Subscribe and unsubscribe from conferences",
        ),
        PermissionDefinition::new(
            "administer notifications",
            "Manage notification settings and queue",
        ),
    ]
}

/// Register notification menu entries.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/user/subscriptions", "My Subscriptions")
            .callback("user_subscriptions")
            .permission("manage own subscriptions"),
    ]
}

/// Render subscribe/unsubscribe toggle on conference item pages.
///
/// Returns an HTML snippet with a subscribe/unsubscribe button.
/// The actual subscription state is checked via the kernel's
/// user_subscriptions table at render time.
#[plugin_tap]
pub fn tap_item_view(item: Item) -> String {
    // Only show subscription toggle for conference items
    if item.item_type != "conference" {
        return String::new();
    }

    // Render a placeholder subscribe button.
    // In production, the route handler would check subscription state
    // and render the appropriate button (subscribe vs unsubscribe).
    format!(
        r#"<div class="subscription-toggle" data-item-id="{}">
            <button class="button button--secondary subscription-toggle__button" type="button">Subscribe</button>
        </div>"#,
        item.id
    )
}

/// Declare the ritrovo_notifications queue for async notification delivery.
#[plugin_tap]
pub fn tap_queue_info() -> serde_json::Value {
    serde_json::json!({
        "queue_name": "ritrovo_notifications",
        "description": "Notification delivery queue for conference subscriptions",
        "max_retries": 3,
        "retry_delay_seconds": 60
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn permissions_count() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 2);
    }

    #[test]
    fn menu_count() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].path, "/user/subscriptions");
    }

    #[test]
    fn view_shows_toggle_for_conference() {
        let item = Item {
            id: Uuid::nil(),
            item_type: "conference".to_string(),
            title: "Test Conf".to_string(),
            fields: HashMap::new(),
            status: 1,
            author_id: Uuid::nil(),
            current_revision_id: None,
            stage_id: live_stage_id(),
            created: 0,
            changed: 0,
            language: None,
        };
        let result = __inner_tap_item_view(item);
        assert!(result.contains("subscription-toggle"));
        assert!(result.contains("Subscribe"));
    }

    #[test]
    fn view_empty_for_non_conference() {
        let item = Item {
            id: Uuid::nil(),
            item_type: "blog".to_string(),
            title: "Test".to_string(),
            fields: HashMap::new(),
            status: 1,
            author_id: Uuid::nil(),
            current_revision_id: None,
            stage_id: live_stage_id(),
            created: 0,
            changed: 0,
            language: None,
        };
        assert!(__inner_tap_item_view(item).is_empty());
    }

    #[test]
    fn queue_info_declares_queue() {
        let info = __inner_tap_queue_info();
        assert_eq!(info["queue_name"], "ritrovo_notifications");
    }
}
