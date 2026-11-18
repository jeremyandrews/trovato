//! B69 test fixture: a plugin that says Deny.
//!
//! No stock plugin answers `tap_item_access` with `Deny`, so nothing in the
//! suite could show that a path honours one. This fixture denies the `edit`
//! operation on every item of type [`DENIED_TYPE`] to everyone the kernel asks
//! about (an administrator never reaches the tap), and is neutral about
//! everything else, so loading it changes nothing for any other type.

use trovato_sdk::prelude::*;

/// The item type whose items this plugin refuses to let anyone edit.
pub const DENIED_TYPE: &str = "test_edit_denied";

#[plugin_tap]
pub fn tap_item_access(input: ItemAccessInput) -> AccessResult {
    if input.item_type == DENIED_TYPE && input.operation == "edit" {
        AccessResult::Deny
    } else {
        AccessResult::Neutral
    }
}
