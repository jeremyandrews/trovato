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

/// Control access to conference items based on editorial stage and operation.
///
/// **View operations** on internal stages:
/// - **Incoming:** Requires `view incoming conferences` or `edit conferences`
/// - **Curated:** Requires `view curated conferences` or `edit conferences`
/// - **Live (or no stage info):** Neutral — let kernel handle public access
///
/// **Non-view operations** (edit, delete, update):
/// - Requires `edit conferences` on any stage
/// - Users with only view permissions on internal stages are denied for mutations
///
/// Non-conference items always return Neutral.
///
/// The kernel already denies anonymous users on internal stages before
/// dispatching to this tap, so we only need to handle authenticated users.
///
/// Note: `publish conferences` is declared in `tap_perm` but not checked here —
/// publish is a workflow transition enforced by the kernel's workflow engine
/// (via `variable.workflow.editorial.yml`), not an item access operation.
#[plugin_tap]
pub fn tap_item_access(input: ItemAccessInput) -> AccessResult {
    // Only control access to conference items
    if input.item_type != "conference" {
        return AccessResult::Neutral;
    }

    // No stage info means live/default — let kernel handle
    let Some(stage) = input.stage_machine_name.as_deref() else {
        return AccessResult::Neutral;
    };

    // Non-view operations (edit, delete, update) require edit conferences
    // regardless of stage. This prevents viewers from mutating content
    // on internal stages even though they can see it.
    if input.operation != "view" {
        return if has_any_permission(&input, &["edit conferences"]) {
            AccessResult::Grant
        } else {
            AccessResult::Deny
        };
    }

    // View operations: check stage-specific permissions
    match stage {
        "incoming" => {
            if has_any_permission(&input, &["view incoming conferences", "edit conferences"]) {
                AccessResult::Grant
            } else {
                AccessResult::Deny
            }
        }
        "curated" => {
            if has_any_permission(&input, &["view curated conferences", "edit conferences"]) {
                AccessResult::Grant
            } else {
                AccessResult::Deny
            }
        }
        // Live/public or unknown stages — let kernel's default logic handle
        _ => AccessResult::Neutral,
    }
}

/// Check if the user has any of the given permissions.
fn has_any_permission(input: &ItemAccessInput, perms: &[&str]) -> bool {
    perms
        .iter()
        .any(|p| input.user_permissions.iter().any(|up| up == p))
}

/// Placeholder for future editor_notes stripping.
///
/// `tap_item_view` receives the `Item` but not the current user context,
/// so we cannot check `edit conferences` here. Once the kernel passes
/// `UserContext` to view taps (planned), this will strip `editor_notes`
/// for non-editors. Until then, returns empty (no extra render HTML).
#[plugin_tap]
pub fn tap_item_view(_item: Item) -> String {
    String::new()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn make_input(
        item_type: &str,
        stage: Option<&str>,
        permissions: &[&str],
        authenticated: bool,
    ) -> ItemAccessInput {
        make_input_op(item_type, stage, permissions, authenticated, "view")
    }

    fn make_input_op(
        item_type: &str,
        stage: Option<&str>,
        permissions: &[&str],
        authenticated: bool,
        operation: &str,
    ) -> ItemAccessInput {
        ItemAccessInput {
            item_id: Uuid::nil(),
            item_type: item_type.to_string(),
            author_id: Uuid::nil(),
            operation: operation.to_string(),
            user_id: Uuid::nil(),
            user_authenticated: authenticated,
            user_permissions: permissions.iter().map(|s| s.to_string()).collect(),
            stage_id: None,
            stage_machine_name: stage.map(|s| s.to_string()),
        }
    }

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
    fn neutral_for_non_conference() {
        let input = make_input("blog", Some("incoming"), &[], true);
        assert_eq!(__inner_tap_item_access(input), AccessResult::Neutral);
    }

    #[test]
    fn neutral_for_live_stage() {
        let input = make_input("conference", Some("live"), &["access content"], true);
        assert_eq!(__inner_tap_item_access(input), AccessResult::Neutral);
    }

    #[test]
    fn neutral_for_no_stage() {
        let input = make_input("conference", None, &["access content"], true);
        assert_eq!(__inner_tap_item_access(input), AccessResult::Neutral);
    }

    #[test]
    fn deny_incoming_without_permission() {
        let input = make_input("conference", Some("incoming"), &["access content"], true);
        assert_eq!(__inner_tap_item_access(input), AccessResult::Deny);
    }

    #[test]
    fn grant_incoming_with_view_permission() {
        let input = make_input(
            "conference",
            Some("incoming"),
            &["access content", "view incoming conferences"],
            true,
        );
        assert_eq!(__inner_tap_item_access(input), AccessResult::Grant);
    }

    #[test]
    fn grant_incoming_with_edit_permission() {
        let input = make_input(
            "conference",
            Some("incoming"),
            &["access content", "edit conferences"],
            true,
        );
        assert_eq!(__inner_tap_item_access(input), AccessResult::Grant);
    }

    #[test]
    fn deny_curated_without_permission() {
        let input = make_input("conference", Some("curated"), &["access content"], true);
        assert_eq!(__inner_tap_item_access(input), AccessResult::Deny);
    }

    #[test]
    fn grant_curated_with_view_permission() {
        let input = make_input(
            "conference",
            Some("curated"),
            &["access content", "view curated conferences"],
            true,
        );
        assert_eq!(__inner_tap_item_access(input), AccessResult::Grant);
    }

    // --- Operation-aware tests (edit/delete/update) ---

    #[test]
    fn deny_edit_incoming_with_only_view_permission() {
        let input = make_input_op(
            "conference",
            Some("incoming"),
            &["access content", "view incoming conferences"],
            true,
            "edit",
        );
        assert_eq!(__inner_tap_item_access(input), AccessResult::Deny);
    }

    #[test]
    fn grant_edit_incoming_with_edit_permission() {
        let input = make_input_op(
            "conference",
            Some("incoming"),
            &["access content", "edit conferences"],
            true,
            "edit",
        );
        assert_eq!(__inner_tap_item_access(input), AccessResult::Grant);
    }

    #[test]
    fn deny_delete_curated_with_only_view_permission() {
        let input = make_input_op(
            "conference",
            Some("curated"),
            &["access content", "view curated conferences"],
            true,
            "delete",
        );
        assert_eq!(__inner_tap_item_access(input), AccessResult::Deny);
    }

    #[test]
    fn grant_delete_curated_with_edit_permission() {
        let input = make_input_op(
            "conference",
            Some("curated"),
            &["access content", "edit conferences"],
            true,
            "delete",
        );
        assert_eq!(__inner_tap_item_access(input), AccessResult::Grant);
    }

    #[test]
    fn deny_update_live_without_edit_permission() {
        let input = make_input_op(
            "conference",
            Some("live"),
            &["access content"],
            true,
            "update",
        );
        assert_eq!(__inner_tap_item_access(input), AccessResult::Deny);
    }

    #[test]
    fn grant_update_live_with_edit_permission() {
        let input = make_input_op(
            "conference",
            Some("live"),
            &["access content", "edit conferences"],
            true,
            "update",
        );
        assert_eq!(__inner_tap_item_access(input), AccessResult::Grant);
    }

    #[test]
    fn neutral_edit_non_conference() {
        let input = make_input_op(
            "blog",
            Some("incoming"),
            &["edit conferences"],
            true,
            "edit",
        );
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
