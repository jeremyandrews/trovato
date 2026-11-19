//! The editorial workflow: which stage moves a user may make.
//!
//! One site configuration variable, [`WORKFLOW_KEY`], holds the transition graph
//! in the shape the tutorial ships:
//!
//! ```yaml
//! key: workflow.editorial
//! value:
//!   transitions:
//!     - {from: incoming, to: curated, permission: "edit any content", label: "Promote to Curated"}
//! ```
//!
//! Stages are named by machine name. [`Workflow::resolve`] turns that into
//! transitions between stage ids, dropping (with a warning) any entry that does
//! not resolve, so an invalid entry is never usable at run time;
//! [`problems`] is the stricter check `config import` applies before anything is
//! stored. The rule a transition grants is applied by
//! `ItemService::may_move`; see `docs/design/stage-workflow.md`.

use std::collections::HashSet;

use serde::Deserialize;
use tracing::warn;
use uuid::Uuid;

use crate::models::stage::Stage;
use crate::tap::UserContext;

/// The site configuration key the kernel reads the workflow from.
pub const WORKFLOW_KEY: &str = "workflow.editorial";

/// One transition as written in configuration, stages by machine name.
#[derive(Debug, Clone, Deserialize)]
struct RawTransition {
    from: String,
    to: String,
    permission: String,
    #[serde(default)]
    label: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RawWorkflow {
    transitions: Vec<RawTransition>,
}

/// A move from one stage to another that holders of `permission` may make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    /// The stage an item leaves.
    pub from: Uuid,
    /// The stage it arrives on.
    pub to: Uuid,
    /// The permission a user must hold to make the move.
    pub permission: String,
    /// What the move is called, for a form or a log.
    pub label: String,
}

/// The resolved transition graph. Empty when nothing is configured, which
/// leaves stage moves to administrators.
#[derive(Debug, Clone, Default)]
pub struct Workflow {
    transitions: Vec<Transition>,
}

fn parse(value: &serde_json::Value) -> Result<RawWorkflow, String> {
    serde_json::from_value::<RawWorkflow>(value.clone()).map_err(|e| {
        format!("`{WORKFLOW_KEY}` must be {{transitions: [{{from, to, permission, label}}]}}: {e}")
    })
}

impl Workflow {
    /// Resolve a stored value against the stages that exist.
    ///
    /// An entry naming a stage that does not exist, or with an empty
    /// permission, is dropped and logged: it is never usable. A value that does
    /// not parse at all is an empty workflow, and is logged.
    pub fn resolve(value: Option<&serde_json::Value>, stages: &[Stage]) -> Self {
        let Some(value) = value else {
            return Self::default();
        };
        let raw = match parse(value) {
            Ok(raw) => raw,
            Err(e) => {
                warn!(error = %e, "ignoring an unreadable workflow: only administrators can move items");
                return Self::default();
            }
        };
        let id_of = |name: &str| stages.iter().find(|s| s.machine_name == name).map(|s| s.id);
        let mut transitions = Vec::new();
        for t in raw.transitions {
            let (Some(from), Some(to)) = (id_of(&t.from), id_of(&t.to)) else {
                warn!(from = %t.from, to = %t.to, "ignoring a workflow transition that names no stage");
                continue;
            };
            if t.permission.trim().is_empty() || from == to {
                warn!(from = %t.from, to = %t.to, "ignoring a workflow transition with no permission or no move");
                continue;
            }
            transitions.push(Transition {
                from,
                to,
                permission: t.permission,
                label: t.label,
            });
        }
        Self { transitions }
    }

    /// The transitions, in configuration order.
    pub fn transitions(&self) -> &[Transition] {
        &self.transitions
    }

    /// Whether the workflow lets `user` move an item from `from` to `to`.
    ///
    /// Only a transition that names exactly this pair counts: none is implied by
    /// another. This is the workflow half of the rule; whether the user may
    /// edit the item at all is the access seam's, and an administrator's
    /// exemption is the caller's (`ItemService::may_move`).
    pub fn allows(&self, from: Uuid, to: Uuid, user: &UserContext) -> bool {
        self.transitions
            .iter()
            .any(|t| t.from == from && t.to == to && user.has_permission(&t.permission))
    }
}

/// What is wrong with a workflow value `config import` is about to store, one
/// message per problem; empty when it is valid.
///
/// `stages` are the machine names that exist or arrive in the same import, and
/// `permissions` every permission name the import accepts for a role (the
/// kernel's, the declared plugin permissions, and those already granted), so a
/// workflow and a role file cannot disagree about what a permission is.
pub fn problems(
    value: &serde_json::Value,
    stages: &HashSet<String>,
    permissions: &HashSet<String>,
) -> Vec<String> {
    let raw = match parse(value) {
        Ok(raw) => raw,
        Err(e) => return vec![e],
    };
    let mut problems = Vec::new();
    for (n, t) in raw.transitions.iter().enumerate() {
        let at = format!("transition {} ({} -> {})", n + 1, t.from, t.to);
        for stage in [&t.from, &t.to] {
            if !stages.contains(stage.as_str()) {
                problems.push(format!("{at}: no stage has the machine name '{stage}'"));
            }
        }
        if t.from == t.to {
            problems.push(format!("{at}: a transition must move to another stage"));
        }
        if t.permission.trim().is_empty() {
            problems.push(format!("{at}: the permission must not be empty"));
        } else if !permissions.contains(&t.permission) {
            problems.push(format!(
                "{at}: unknown permission '{}': it is not one the kernel defines, no enabled \
                 plugin declares it, and no role holds it",
                t.permission
            ));
        }
    }
    problems
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::models::stage::StageVisibility;
    use serde_json::json;

    fn stage(name: &str) -> Stage {
        Stage {
            id: Uuid::now_v7(),
            label: name.to_string(),
            description: None,
            machine_name: name.to_string(),
            visibility: StageVisibility::Internal,
            is_default: false,
            weight: 0,
            created: 0,
            changed: 0,
        }
    }

    fn user(permissions: &[&str]) -> UserContext {
        UserContext::authenticated(
            Uuid::now_v7(),
            permissions.iter().map(|p| (*p).to_string()).collect(),
        )
    }

    #[test]
    fn a_transition_allows_exactly_its_pair_to_holders_of_its_permission() {
        let stages = [stage("a"), stage("b"), stage("c")];
        let (a, b, c) = (stages[0].id, stages[1].id, stages[2].id);
        let workflow = Workflow::resolve(
            Some(&json!({"transitions": [
                {"from": "a", "to": "b", "permission": "p"},
                {"from": "b", "to": "c", "permission": "p"},
            ]})),
            &stages,
        );
        assert!(workflow.allows(a, b, &user(&["p"])));
        assert!(
            !workflow.allows(a, b, &user(&["q"])),
            "needs the permission"
        );
        assert!(
            !workflow.allows(a, c, &user(&["p"])),
            "a -> c is not implied"
        );
        assert!(!workflow.allows(b, a, &user(&["p"])), "nor is the reverse");
    }

    #[test]
    fn entries_that_do_not_resolve_are_never_usable() {
        let stages = [stage("a"), stage("b")];
        let workflow = Workflow::resolve(
            Some(&json!({"transitions": [
                {"from": "a", "to": "gone", "permission": "p"},
                {"from": "a", "to": "b", "permission": " "},
                {"from": "a", "to": "a", "permission": "p"},
            ]})),
            &stages,
        );
        assert!(workflow.transitions().is_empty());
        assert!(
            Workflow::resolve(Some(&json!("junk")), &stages)
                .transitions()
                .is_empty()
        );
        assert!(Workflow::resolve(None, &stages).transitions().is_empty());
    }

    #[test]
    fn import_problems_name_the_transition() {
        let stages: HashSet<String> = ["a", "b"].iter().map(|s| (*s).to_string()).collect();
        let permissions: HashSet<String> = ["p"].iter().map(|s| (*s).to_string()).collect();
        assert!(problems(
            &json!({"transitions": [{"from": "a", "to": "b", "permission": "p", "label": "Go"}]}),
            &stages,
            &permissions
        )
        .is_empty());
        let found = problems(
            &json!({"transitions": [{"from": "a", "to": "x", "permission": "q"}]}),
            &stages,
            &permissions,
        );
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("'x'"));
        assert!(found[1].contains("unknown permission 'q'"));
        assert_eq!(problems(&json!([]), &stages, &permissions).len(), 1);
    }
}
