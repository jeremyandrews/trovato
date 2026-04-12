//! Trovato Page Builder plugin.
//!
//! Provides visual drag-and-drop page composition using Puck as the editor
//! and Tera templates for server-side rendering. Supports 12 component types
//! covering heroes, columns, cards, CTAs, accordions, and more.

use trovato_sdk::prelude::*;

/// Register page builder permissions.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition {
        name: "use page builder".into(),
        description: "Create and edit pages using the visual drag-and-drop page builder".into(),
    }]
}

/// Register page builder menu items.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![]
}
