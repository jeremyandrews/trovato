//! Block editor plugin for Trovato.
//!
//! This plugin serves as the feature flag and permission provider for the
//! Editor.js block editing system. When enabled, it:
//!
//! - Provides the `"use block editor"` permission
//! - Gates the `/api/block-editor/upload` and `/api/block-editor/preview` routes
//! - Enables the block editor widget in content forms for `FieldType::Blocks` fields
//!
//! The kernel provides all infrastructure (block rendering, upload handling,
//! `BlockTypeRegistry`). This plugin activates it.

use trovato_sdk::prelude::*;

/// Register block editor permissions.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        "use block editor",
        "Use the block editor for content with Blocks fields",
    )]
}
