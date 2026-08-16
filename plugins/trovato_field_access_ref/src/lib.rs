//! FR-8 reference plugin — the canonical `tap_field_access` implementation.
//!
//! Built with the real `trovato-plugin-sdk` and the real `wasm32-wasip1`
//! toolchain, this is a **real, enableable** plugin (not a test-only fixture,
//! per design amendment β). It exists to:
//!
//! 1. **Validate the frozen `tap_field_access` batch schema before PF-5** (the
//!    tap-csp-alter / Story-2.4 precedent — never freeze an unexercised payload).
//!    Story 3.8's integration test drives this plugin through the real kernel
//!    `TapDispatcher`, round-tripping `FieldAccessBatchInput` →
//!    `FieldAccessBatchResult`.
//! 2. **Document the two downstream patterns** every FR-8 consumer (Cairn,
//!    Argus, Ritrovo) builds on, both **type-level** (D-22):
//!    - **Ritrovo role pattern** — a field on a type requires a permission to
//!      view; a viewer lacking it is denied that field (`role_rules`).
//!    - **Cairn encryption-tier pattern** — a field on a type carries a
//!      sensitivity tier; a viewer whose clearance is below the tier is denied
//!      (`tier_rules`). A field's tier is a property of the field *definition on
//!      the type*, not of the individual item — which is exactly why type-level
//!      granularity suffices.
//!
//! Rules are read from the plugin's own `variables` config (key `field_rules`),
//! with the baked-in [`DEFAULT_RULES`] as the fallback. Because the kernel
//! flushes the shared field-access cache on a `variables` write (design
//! amendment α), an admin editing `field_rules` takes effect on the next
//! request — no ≤5-minute staleness window.
//!
//! # Aggregation contract this plugin honours
//!
//! It returns `Deny` for a governed field the viewer may not see, `Allow` for a
//! governed field the viewer may see, and **omits** ungoverned fields (an absent
//! field is `NoOpinion`, which the kernel defaults to visible — fail-open). The
//! kernel aggregates `Deny`-wins across all implementing plugins.

use std::collections::HashMap;

use serde::Deserialize;
use trovato_sdk::plugin_tap;
use trovato_sdk::types::{
    FieldAccessBatchInput, FieldAccessBatchResult, FieldAccessResult, PermissionDefinition,
};

/// Default field-access rules, used when the `field_rules` variable is unset or
/// malformed. Two example types demonstrate the two patterns:
///
/// - `person`: `ssn` needs `"view pii"`, `salary` needs `"view salary"`
///   (Ritrovo role pattern).
/// - `record`: `secret_notes` is tier 3, `top_secret` is tier 5; a viewer needs
///   clearance ≥ the tier (Cairn encryption-tier pattern).
pub const DEFAULT_RULES: &str = r#"{
  "role_rules": {
    "person": { "ssn": "view pii", "salary": "view salary" }
  },
  "tier_rules": {
    "record": { "secret_notes": 3, "top_secret": 5 }
  }
}"#;

/// The plugin's rule set: `item_type -> field -> requirement`, split by pattern.
#[derive(Debug, Default, Deserialize)]
struct Rules {
    /// Ritrovo role pattern: `type -> field -> required permission`.
    #[serde(default)]
    role_rules: HashMap<String, HashMap<String, String>>,
    /// Cairn encryption-tier pattern: `type -> field -> minimum clearance tier`.
    #[serde(default)]
    tier_rules: HashMap<String, HashMap<String, u32>>,
}

/// Load the rules from the `field_rules` variable, falling back to
/// [`DEFAULT_RULES`] on a read error or a parse failure.
fn load_rules() -> Rules {
    let raw = trovato_sdk::host::variables_get("field_rules", DEFAULT_RULES)
        .unwrap_or_else(|_| DEFAULT_RULES.to_string());
    serde_json::from_str(&raw)
        .or_else(|_| serde_json::from_str(DEFAULT_RULES))
        .unwrap_or_default()
}

/// The viewer's clearance = the highest `N` over permissions of the form
/// `"clearance N"` (0 if none). Type-level: derived purely from the viewer's
/// permission set, no per-item data.
fn viewer_clearance(permissions: &[String]) -> u32 {
    permissions
        .iter()
        .filter_map(|p| p.strip_prefix("clearance "))
        .filter_map(|n| n.trim().parse::<u32>().ok())
        .max()
        .unwrap_or(0)
}

/// Reference `tap_field_access`: decide a batch of fields for one type/operation,
/// type-level, deny-wins-compatible.
#[plugin_tap]
pub fn tap_field_access(input: FieldAccessBatchInput) -> FieldAccessBatchResult {
    let rules = load_rules();
    let permissions = &input.user.permissions;
    let clearance = viewer_clearance(permissions);
    let mut decisions: HashMap<String, FieldAccessResult> = HashMap::new();

    // Ritrovo role pattern: governed field ⇒ Allow iff the viewer holds the
    // required permission, else Deny. Ungoverned fields are left NoOpinion.
    if let Some(type_roles) = rules.role_rules.get(&input.item_type) {
        for field in &input.fields {
            if let Some(required) = type_roles.get(field) {
                let verdict = if permissions.iter().any(|p| p == required) {
                    FieldAccessResult::Allow
                } else {
                    FieldAccessResult::Deny
                };
                decisions.insert(field.clone(), verdict);
            }
        }
    }

    // Cairn encryption-tier pattern: governed field ⇒ Allow iff clearance ≥ tier,
    // else Deny. Deny-wins if a field is governed by both patterns.
    if let Some(type_tiers) = rules.tier_rules.get(&input.item_type) {
        for field in &input.fields {
            if let Some(&tier) = type_tiers.get(field) {
                let verdict = if clearance >= tier {
                    FieldAccessResult::Allow
                } else {
                    FieldAccessResult::Deny
                };
                // Deny-wins locally too: never upgrade an existing Deny to Allow.
                let entry = decisions.entry(field.clone()).or_insert(verdict.clone());
                if *entry != FieldAccessResult::Deny {
                    *entry = verdict;
                }
            }
        }
    }

    FieldAccessBatchResult { decisions }
}

/// Declare the example permissions the rules reference, so an admin can grant
/// them (and so this plugin reads as a complete, self-contained reference).
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new("view pii", "View PII fields (e.g. SSN)"),
        PermissionDefinition::new("view salary", "View salary fields"),
        PermissionDefinition::new("clearance 3", "Security clearance tier 3"),
        PermissionDefinition::new("clearance 5", "Security clearance tier 5"),
    ]
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
    fn role_pattern_denies_without_permission() {
        // No "view pii" ⇒ ssn denied; has "view salary" ⇒ salary allowed;
        // ungoverned "bio" ⇒ NoOpinion (absent).
        let r =
            __inner_tap_field_access(input("person", &["view salary"], &["ssn", "salary", "bio"]));
        assert_eq!(r.decisions.get("ssn"), Some(&FieldAccessResult::Deny));
        assert_eq!(r.decisions.get("salary"), Some(&FieldAccessResult::Allow));
        assert!(!r.decisions.contains_key("bio"), "ungoverned ⇒ NoOpinion");
    }

    #[test]
    fn tier_pattern_denies_below_clearance() {
        // clearance 3 ⇒ secret_notes (tier 3) allowed, top_secret (tier 5) denied.
        let r = __inner_tap_field_access(input(
            "record",
            &["clearance 3"],
            &["secret_notes", "top_secret", "summary"],
        ));
        assert_eq!(
            r.decisions.get("secret_notes"),
            Some(&FieldAccessResult::Allow)
        );
        assert_eq!(
            r.decisions.get("top_secret"),
            Some(&FieldAccessResult::Deny)
        );
        assert!(!r.decisions.contains_key("summary"));
    }

    #[test]
    fn no_clearance_denies_all_tiered_fields() {
        let r = __inner_tap_field_access(input("record", &[], &["secret_notes", "top_secret"]));
        assert_eq!(
            r.decisions.get("secret_notes"),
            Some(&FieldAccessResult::Deny)
        );
        assert_eq!(
            r.decisions.get("top_secret"),
            Some(&FieldAccessResult::Deny)
        );
    }

    #[test]
    fn unknown_type_has_no_opinion() {
        let r = __inner_tap_field_access(input("article", &[], &["title", "body"]));
        assert!(r.decisions.is_empty(), "no rules for type ⇒ all NoOpinion");
    }

    #[test]
    fn viewer_clearance_takes_the_max() {
        assert_eq!(
            viewer_clearance(&["clearance 2".into(), "clearance 5".into()]),
            5
        );
        assert_eq!(viewer_clearance(&["access content".into()]), 0);
    }
}
