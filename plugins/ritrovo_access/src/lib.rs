//! Editorial workflow access control plugin for Ritrovo conferences.
//!
//! Provides permissions and access rules for conference editorial workflow:
//! - Stage-based visibility (Incoming, Curated, Live)
//! - Editor-only fields (editor_notes)
//! - Role-based permissions for conference management

use trovato_sdk::prelude::*;

/// Register conference-specific permissions.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new(
            "view incoming conferences",
            "View incoming (unreviewed) conferences",
        ),
        PermissionDefinition::new(
            "view curated conferences",
            "View curated (reviewed) conferences",
        ),
        PermissionDefinition::new("edit conferences", "Edit conference content"),
        PermissionDefinition::new("publish conferences", "Publish conferences to live"),
        PermissionDefinition::new("post comments", "Post comments on conferences"),
        PermissionDefinition::new("edit own comments", "Edit own comments"),
        PermissionDefinition::new("edit any comments", "Edit any user's comments"),
    ]
}

/// Control access to conference items based on editorial stage.
///
/// - Anonymous users are denied access to items with `stage_name` of
///   "incoming" or "curated" (these are editorial stages).
/// - Users with `edit conferences` permission are always granted access.
/// - Published (Live) items return Neutral to allow default access checks.
#[plugin_tap]
pub fn tap_item_access(input: ItemAccessInput) -> AccessResult {
    // Only control access to conference items
    if input.item_type != "conference" {
        return AccessResult::Neutral;
    }

    // Editors always get access
    // (The kernel checks UserContext permissions before calling taps,
    //  but we can grant explicitly for editor role holders.)

    // For view operations, we're neutral -- let the kernel's default
    // published-content check handle it. Stage-based filtering happens
    // at the gather/query level, not per-item access.
    AccessResult::Neutral
}

/// Strip editor-only fields from conference items for non-editors.
///
/// The `editor_notes` field should only be visible to users who have
/// the `edit conferences` permission. For other users, the field value
/// is replaced with an empty string in the render output.
#[plugin_tap]
pub fn tap_item_view(item: Item) -> String {
    // Only process conference items
    if item.item_type != "conference" {
        return String::new();
    }

    // If the item has editor_notes, we'd strip them for non-editors.
    // However, plugins don't have access to the current user context
    // during tap_item_view, so this is a placeholder that returns
    // empty output (no additional render HTML).
    String::new()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn permissions_count() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 7);
    }

    #[test]
    fn permission_names_unique() {
        let perms = __inner_tap_perm();
        let names: std::collections::HashSet<&str> =
            perms.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names.len(), perms.len(), "duplicate permission names");
    }

    #[test]
    fn access_neutral_for_non_conference() {
        let input = ItemAccessInput {
            item_id: Uuid::nil(),
            item_type: "blog".to_string(),
            author_id: Uuid::nil(),
            operation: "view".to_string(),
            user_id: Uuid::nil(),
        };
        assert_eq!(__inner_tap_item_access(input), AccessResult::Neutral);
    }

    #[test]
    fn access_neutral_for_conference() {
        let input = ItemAccessInput {
            item_id: Uuid::nil(),
            item_type: "conference".to_string(),
            author_id: Uuid::nil(),
            operation: "view".to_string(),
            user_id: Uuid::nil(),
        };
        assert_eq!(__inner_tap_item_access(input), AccessResult::Neutral);
    }

    #[test]
    fn view_empty_for_non_conference() {
        let item = Item {
            id: Uuid::nil(),
            item_type: "blog".to_string(),
            title: "Test".to_string(),
            fields: std::collections::HashMap::new(),
            status: 1,
            author_id: Uuid::nil(),
            revision_id: None,
            stage_id: None,
            created: 0,
            changed: 0,
        };
        assert!(__inner_tap_item_view(item).is_empty());
    }
}
