//! Scheduled publishing plugin for Trovato.
//!
//! Allows scheduling items for future publish/unpublish via
//! field_publish_on and field_unpublish_on JSONB fields.
//!
//! Implements `tap_cron` to process scheduled publish/unpublish
//! operations each cron cycle using DB host functions.

use trovato_sdk::host;
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

/// Process scheduled publish/unpublish operations.
///
/// Called each cron cycle. Publishes items where `field_publish_on` <= now
/// and unpublishes items where `field_unpublish_on` <= now.
#[plugin_tap]
pub fn tap_cron(input: CronInput) -> serde_json::Value {
    let now = input.timestamp;

    let published = host::execute_raw(
        "UPDATE item SET status = 1, changed = $1 \
         WHERE status = 0 \
         AND fields->>'field_publish_on' IS NOT NULL \
         AND (fields->>'field_publish_on') ~ '^[0-9]+$' \
         AND (fields->>'field_publish_on')::bigint <= $1",
        &[serde_json::json!(now)],
    )
    .unwrap_or(0);

    let unpublished = host::execute_raw(
        "UPDATE item SET status = 0, changed = $1 \
         WHERE status = 1 \
         AND fields->>'field_unpublish_on' IS NOT NULL \
         AND (fields->>'field_unpublish_on') ~ '^[0-9]+$' \
         AND (fields->>'field_unpublish_on')::bigint <= $1",
        &[serde_json::json!(now)],
    )
    .unwrap_or(0);

    serde_json::json!({"published": published, "unpublished": unpublished})
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
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

    #[test]
    fn tap_cron_returns_counts() {
        let input = CronInput {
            timestamp: 1_700_000_000,
        };
        let result = __inner_tap_cron(input);
        // Stub host functions return 0 for both
        assert_eq!(result["published"], 0);
        assert_eq!(result["unpublished"], 0);
    }
}
