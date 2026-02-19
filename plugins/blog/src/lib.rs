//! Blog plugin for Trovato.
//!
//! Provides a "blog" content type with body and tags fields.
//! Demonstrates the SDK's `#[plugin_tap]` proc macro for tap registration.

use trovato_sdk::prelude::*;

/// Content type definitions for blog posts.
///
/// Called during plugin initialization to register content types.
#[plugin_tap]
pub fn tap_item_info() -> Vec<ContentTypeDefinition> {
    vec![ContentTypeDefinition {
        machine_name: "blog".into(),
        label: "Blog Post".into(),
        description: "A blog entry with body and tags".into(),
        fields: vec![
            FieldDefinition::new("field_body", FieldType::TextLong)
                .required()
                .label("Body"),
            FieldDefinition::new(
                "field_tags",
                FieldType::RecordReference("category_term".into()),
            )
            .cardinality(-1)
            .label("Tags"),
        ],
    }]
}

/// Permissions provided by the blog plugin.
///
/// Uses standard CRUD permissions matching the kernel fallback format.
/// "edit blog content" / "delete blog content" serve as "edit any" / "delete any"
/// permissions. Author access ("own" semantics) is handled by `tap_item_access`
/// below, which grants access when user == author.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    PermissionDefinition::crud_for_type("blog")
}

/// Access control for blog posts — implements "own" semantics.
///
/// The kernel flow for non-admin access checks:
/// 1. Published + "view" → kernel shortcut grants if user has "access content"
/// 2. `tap_item_access` → this function (below)
/// 3. Permission fallback → checks `"{operation} blog content"`
///
/// This tap grants access when user == author, providing "own" semantics:
/// - Authors can view their own unpublished drafts
/// - Authors can edit and delete their own posts
///
/// Non-authors fall through to the kernel permission fallback, which checks
/// "edit blog content" / "delete blog content" — the "any" equivalent.
///
/// Note: The WASM boundary prevents checking user permissions inside taps
/// (ItemAccessInput has no permission data). Author access is therefore
/// unconditional — any authenticated author can edit/delete their own posts.
#[plugin_tap]
pub fn tap_item_access(input: ItemAccessInput) -> AccessResult {
    if input.item_type != "blog" {
        return AccessResult::Neutral;
    }

    // Author can always access their own posts (view drafts, edit, delete)
    if input.user_id == input.author_id {
        return AccessResult::Grant;
    }

    // Non-authors: defer to kernel permission fallback ("edit blog content", etc.)
    AccessResult::Neutral
}

/// Menu routes provided by the blog plugin.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/blog", "Blog")
            .callback("blog_listing")
            .permission("access content"),
        MenuDefinition::new("/blog/:slug", "Post")
            .callback("blog_view")
            .permission("access content"),
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn item_info_returns_one_type() {
        let types = __inner_tap_item_info();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].machine_name, "blog");
    }

    #[test]
    fn perm_returns_four_permissions() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 4); // 4 per type × 1 type (view/create/edit/delete)
    }

    #[test]
    fn perm_format_matches_kernel_fallback() {
        let perms = __inner_tap_perm();
        for perm in &perms {
            assert!(
                perm.name.ends_with(" blog content"),
                "permission '{}' must end with 'blog content'",
                perm.name
            );
        }
    }

    #[test]
    fn access_neutral_for_non_blog() {
        let input = ItemAccessInput {
            item_id: Uuid::nil(),
            item_type: "page".into(),
            author_id: Uuid::nil(),
            operation: "edit".into(),
            user_id: Uuid::nil(),
        };
        assert_eq!(__inner_tap_item_access(input), AccessResult::Neutral);
    }

    #[test]
    fn access_grant_for_author() {
        let author = Uuid::nil();
        let input = ItemAccessInput {
            item_id: Uuid::nil(),
            item_type: "blog".into(),
            author_id: author,
            operation: "edit".into(),
            user_id: author,
        };
        assert_eq!(__inner_tap_item_access(input), AccessResult::Grant);
    }

    #[test]
    fn access_neutral_for_non_author() {
        let input = ItemAccessInput {
            item_id: Uuid::nil(),
            item_type: "blog".into(),
            author_id: Uuid::from_u128(1),
            operation: "edit".into(),
            user_id: Uuid::from_u128(2),
        };
        assert_eq!(__inner_tap_item_access(input), AccessResult::Neutral);
    }

    #[test]
    fn access_grant_for_author_view() {
        let author = Uuid::nil();
        let input = ItemAccessInput {
            item_id: Uuid::nil(),
            item_type: "blog".into(),
            author_id: author,
            operation: "view".into(),
            user_id: author,
        };
        assert_eq!(__inner_tap_item_access(input), AccessResult::Grant);
    }

    #[test]
    fn access_grant_for_author_delete() {
        let author = Uuid::nil();
        let input = ItemAccessInput {
            item_id: Uuid::nil(),
            item_type: "blog".into(),
            author_id: author,
            operation: "delete".into(),
            user_id: author,
        };
        assert_eq!(__inner_tap_item_access(input), AccessResult::Grant);
    }

    #[test]
    fn menu_returns_two_routes() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 2);
        assert_eq!(menus[0].path, "/blog");
    }
}
