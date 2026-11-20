//! B74 fixture: plugin navigation entries with a parent.
//!
//! Three entries under `/user`, the account menu, each standing for one rule
//! the kernel applies before a plugin entry reaches a template:
//!
//! - `/test-menu-entries/extra` needs [`PERM_EXTRA`], so it is drawn for a
//!   viewer holding it and for nobody else;
//! - `/user/{uid}/test-menu-entries` is the signed-in viewer's own page, drawn
//!   with their id in place of `{uid}` and never for an anonymous visitor;
//! - `/test-menu-entries/{slug}` names a segment nobody can fill, so it is a
//!   link no one can follow and is never drawn.
//!
//! All three are navigation only (`MenuRoute::page`); none is served.

use trovato_sdk::prelude::*;
use trovato_sdk::types::MenuRoute;

/// The permission the gated entry requires.
pub const PERM_EXTRA: &str = "see test menu extra";

/// Declare the permission the gated entry requires.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        PERM_EXTRA,
        "See the gated test entry in the account menu",
    )]
}

/// Register the three account menu entries.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuRoute> {
    vec![
        MenuRoute::page("/test-menu-entries/extra", "Test menu extra")
            .parent("/user")
            .permission(PERM_EXTRA)
            .weight(1),
        MenuRoute::page("/user/{uid}/test-menu-entries", "Test menu mine")
            .parent("/user")
            .permission("")
            .weight(2),
        MenuRoute::page("/test-menu-entries/{slug}", "Test menu unfollowable")
            .parent("/user")
            .permission("")
            .weight(3),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_sits_under_the_account_menu() {
        let entries = __inner_tap_menu();
        assert_eq!(entries.len(), 3);
        assert!(
            entries
                .iter()
                .all(|e| e.parent.as_deref() == Some("/user") && e.visible)
        );
    }
}
