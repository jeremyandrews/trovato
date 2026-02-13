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
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new("create blog content", "Create new blog posts"),
        PermissionDefinition::new("edit own blog content", "Edit own blog posts"),
        PermissionDefinition::new("delete own blog content", "Delete own blog posts"),
    ]
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

/// Access control for blog items.
#[plugin_tap]
pub fn tap_item_access(input: ItemAccessInput) -> AccessResult {
    // Only handle blog items
    if input.item.item_type != "blog" {
        return AccessResult::Neutral;
    }

    // Published posts are accessible to all
    if input.item.status == 1 && input.op == "view" {
        return AccessResult::Grant;
    }

    // Otherwise neutral - let permission system decide
    AccessResult::Neutral
}

/// Input for tap_item_access.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ItemAccessInput {
    pub item: Item,
    pub op: String,
}
