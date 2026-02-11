//! Blog plugin for Trovato.
//!
//! Provides a "blog" content type with body and tags fields.
//! This is a reference plugin demonstrating the SDK's handle-based API.
//!
//! Once proc macros are implemented (Phase 2), this file will use
//! `#[plugin_info]` and `#[plugin_tap]` attributes. For now it defines
//! the types and logic that those macros will wrap.

use trovato_sdk::prelude::*;

/// Content type definition for blog posts.
pub fn item_info() -> Vec<ContentTypeDefinition> {
    vec![ContentTypeDefinition {
        machine_name: "blog".into(),
        label: "Blog Post".into(),
        description: "A blog entry with body and tags".into(),
        fields: vec![
            FieldDefinition::new("field_body", FieldType::TextLong)
                .required()
                .label("Body"),
            FieldDefinition::new("field_tags", FieldType::RecordReference("category_term".into()))
                .cardinality(-1)
                .label("Tags"),
        ],
    }]
}

/// Permissions provided by the blog plugin.
pub fn perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new("create blog content", "Create new blog posts"),
        PermissionDefinition::new("edit own blog content", "Edit own blog posts"),
        PermissionDefinition::new("delete own blog content", "Delete own blog posts"),
    ]
}

/// Menu routes provided by the blog plugin.
pub fn menu() -> Vec<MenuDefinition> {
    vec![MenuDefinition {
        path: "blog".into(),
        title: "Blog".into(),
        callback: "blog_listing".into(),
        permission: "access content".into(),
        parent: None,
    }]
}
