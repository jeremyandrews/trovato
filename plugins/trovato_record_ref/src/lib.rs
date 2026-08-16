//! P11g / D-59 reference plugin — a lightweight-record type governed by the
//! **same** FR-8 `tap_field_access` seam that governs Items.
//!
//! Built with the real `trovato-plugin-sdk` and the real `wasm32-wasip1`
//! toolchain, this is a real, enableable plugin (the tap-csp-alter / Story-2.4
//! discipline — never freeze an unexercised payload). It exists to prove the
//! **non-negotiable D-55 fence**: a plugin's `tap_field_access` governs a
//! lightweight record exactly as it governs an Item — deny-wins, fail-open, one
//! seam, no second access path.
//!
//! The plugin declares the `event_record` lightweight-record type (in its
//! manifest) and implements `tap_field_access` over that type's **logical field
//! names** (the field-map keys the kernel dispatches on): `secret_notes` requires
//! the `"view secret_notes"` permission; a viewer without it is denied that
//! field, while `location` and `capacity` stay `NoOpinion` (fail-open, visible).
//! This is the Ritrovo role pattern from the FR-8 reference plugin, now applied
//! to a lightweight record instead of an Item — identical mechanics.
//!
//! `crates/kernel/tests/record_gather_fixture_test.rs` drives this plugin through
//! a real `GatherService` over the real `record_event` table and the real
//! `TapDispatcher`, asserting that a low-privilege viewer's gather page hides
//! `secret_notes` while a permitted viewer's shows it — the record tier consuming
//! the frozen D-22..D-24 semantics unchanged.

use std::collections::HashMap;

use trovato_sdk::plugin_tap;
use trovato_sdk::types::{
    FieldAccessBatchInput, FieldAccessBatchResult, FieldAccessResult, PermissionDefinition,
};

/// The record type this plugin governs — matches the `[[record_types]]` name in
/// the manifest and the `item_type` the kernel dispatches `tap_field_access`
/// with for a lightweight-record gather.
const RECORD_TYPE: &str = "event_record";

/// The governed logical field and the permission it requires (Ritrovo role
/// pattern). A viewer holding the permission sees it; everyone else is denied.
const GOVERNED_FIELD: &str = "secret_notes";
const REQUIRED_PERMISSION: &str = "view secret_notes";

/// Reference `tap_field_access` for the `event_record` lightweight-record type.
///
/// Type-level and deny-wins-compatible, identical to the Item field-access
/// contract: return `Deny` for the governed field when the viewer lacks the
/// permission, `Allow` when it holds it, and **omit** every ungoverned field
/// (absent ⇒ `NoOpinion` ⇒ the kernel's fail-open default keeps it visible).
#[plugin_tap]
pub fn tap_field_access(input: FieldAccessBatchInput) -> FieldAccessBatchResult {
    let mut decisions: HashMap<String, FieldAccessResult> = HashMap::new();

    // Only govern our own record type; NoOpinion on everything else.
    if input.item_type != RECORD_TYPE {
        return FieldAccessBatchResult { decisions };
    }

    let has_permission = input
        .user
        .permissions
        .iter()
        .any(|p| p == REQUIRED_PERMISSION);

    for field in &input.fields {
        if field == GOVERNED_FIELD {
            let verdict = if has_permission {
                FieldAccessResult::Allow
            } else {
                FieldAccessResult::Deny
            };
            decisions.insert(field.clone(), verdict);
        }
        // location / capacity: ungoverned ⇒ NoOpinion (omitted).
    }

    FieldAccessBatchResult { decisions }
}

/// Declare the permission the rule references, so an admin can grant it and the
/// plugin reads as a complete, self-contained reference.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![PermissionDefinition::new(
        REQUIRED_PERMISSION,
        "View the secret_notes field of event records",
    )]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use trovato_sdk::types::FieldAccessUser;

    fn input(item_type: &str, perms: &[&str], fields: &[&str]) -> FieldAccessBatchInput {
        FieldAccessBatchInput {
            user: FieldAccessUser {
                user_id: uuid::Uuid::nil(),
                authenticated: true,
                permissions: perms.iter().map(|s| s.to_string()).collect(),
            },
            item_type: item_type.to_string(),
            operation: "view".to_string(),
            fields: fields.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn denies_secret_notes_without_permission() {
        let r = __inner_tap_field_access(input(
            RECORD_TYPE,
            &[],
            &["location", "capacity", "secret_notes"],
        ));
        assert_eq!(
            r.decisions.get("secret_notes"),
            Some(&FieldAccessResult::Deny)
        );
        assert!(
            !r.decisions.contains_key("location"),
            "ungoverned ⇒ NoOpinion"
        );
        assert!(
            !r.decisions.contains_key("capacity"),
            "ungoverned ⇒ NoOpinion"
        );
    }

    #[test]
    fn allows_secret_notes_with_permission() {
        let r = __inner_tap_field_access(input(
            RECORD_TYPE,
            &["view secret_notes"],
            &["secret_notes"],
        ));
        assert_eq!(
            r.decisions.get("secret_notes"),
            Some(&FieldAccessResult::Allow)
        );
    }

    #[test]
    fn no_opinion_on_other_types() {
        let r = __inner_tap_field_access(input("person", &[], &["secret_notes"]));
        assert!(
            r.decisions.is_empty(),
            "not our record type ⇒ all NoOpinion"
        );
    }
}
