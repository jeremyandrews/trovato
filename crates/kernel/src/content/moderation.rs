//! Content moderation: a site's own editorial workflow (B158, B301).
//!
//! A content type whose settings say `moderation: true` is moderated by one
//! workflow, named in the type's `moderation_workflow` setting (the built-in
//! `editorial` workflow when the setting is absent). Every workflow a site
//! defines lives in one site configuration variable, [`WORKFLOWS_KEY`]
//! (`moderation.workflows`): its states, which of them are published, which
//! one a new item starts on, and its transitions, each granted by its own
//! permission (`use <workflow id> transition <id>`, named as Drupal's
//! Editorial workflow names its own, so a migrated role keeps its grants on
//! an upgraded site, which is seeded with exactly [`builtin_editorial`]).
//!
//! A revision carries its state in `item_revision.moderation_state`. A state
//! with `default_revision: false` (Draft, in the built-in workflow) means a
//! save into it over a published item is a forward revision: recorded, with
//! the item row — what visitors see — left as it is. A state with
//! `published: true` must have `default_revision: true`: publishing always
//! replaces what visitors see. A save is a transition from the state of the
//! item's latest revision to the state the user asks for, and [`ItemService`]
//! (`super::ItemService`) applies it on every write path; this module is pure.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::tap::UserContext;

/// A revision being worked on, not visible to visitors: the built-in
/// workflow's starting state.
pub const DRAFT: &str = "draft";
/// The revision visitors see: the built-in workflow's one published state.
pub const PUBLISHED: &str = "published";
/// Taken down: unpublished, kept: the built-in workflow's other terminal
/// state.
pub const ARCHIVED: &str = "archived";

/// The type setting that turns moderation on.
pub const SETTING: &str = "moderation";

/// The type setting naming the workflow a moderated type uses. Absent means
/// [`EDITORIAL`].
pub const WORKFLOW_SETTING: &str = "moderation_workflow";

/// The site configuration key every workflow a site defines is stored under.
pub const WORKFLOWS_KEY: &str = "moderation.workflows";

/// The built-in workflow's id, and what a type's `moderation_workflow`
/// setting defaults to when absent.
pub const EDITORIAL: &str = "editorial";

/// The permission that lets a user read the latest revision of an item they
/// may view, when it is a draft ahead of the published one, and the moderated
/// content list (Drupal's `view latest version`).
pub const VIEW_LATEST_VERSION: &str = "view latest version";

/// One state a revision can be in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// The machine name, stored in `item_revision.moderation_state`: at most
    /// 32 characters, matching `^[a-z][a-z0-9_]*$`.
    pub id: String,
    /// What a person calls it.
    pub label: String,
    /// Whether an item in this state is published: what visitors with
    /// `access content` see. A published state must have `default_revision:
    /// true`.
    #[serde(default)]
    pub published: bool,
    /// Whether a save into this state over a published item replaces the
    /// item row (what visitors see) rather than waiting beside it as a
    /// forward revision. Published and Archived are `true` in the built-in
    /// workflow; Draft is `false`.
    #[serde(default)]
    pub default_revision: bool,
}

/// One move from one or more states to another, granted by its own
/// permission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    /// The machine name, which the permission carries.
    pub id: String,
    /// What a person calls it.
    pub label: String,
    /// The states it leaves from.
    pub from: Vec<String>,
    /// The state it arrives at.
    pub to: String,
}

/// A site's editorial workflow: its states, in the order a form offers them,
/// and the transitions between them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModerationWorkflow {
    /// The machine name, which every transition's permission carries (`use
    /// <id> transition <transition id>`).
    pub id: String,
    /// What a person calls it.
    pub label: String,
    /// The state a new item starts on.
    pub default_state: String,
    /// Every state, in the order a form offers them.
    pub states: Vec<State>,
    /// Every transition.
    pub transitions: Vec<Transition>,
}

/// The variable's stored shape: `{workflows: [...]}`.
#[derive(Debug, Clone, Deserialize)]
struct RawWorkflows {
    workflows: Vec<ModerationWorkflow>,
}

impl ModerationWorkflow {
    /// The state named `id`, if the workflow has one.
    pub fn state(&self, id: &str) -> Option<&State> {
        self.states.iter().find(|s| s.id == id)
    }

    /// What a person calls the state named `id`, or "Unknown" when the
    /// workflow has none by that name.
    pub fn label(&self, id: &str) -> String {
        self.state(id)
            .map_or_else(|| "Unknown".to_string(), |s| s.label.clone())
    }

    /// The transition from `from` to `to`, if the workflow has one.
    pub fn transition(&self, from: &str, to: &str) -> Option<&Transition> {
        self.transitions
            .iter()
            .find(|t| t.to == to && t.from.iter().any(|f| f == from))
    }

    /// The permission that grants `transition`: `use <workflow id> transition
    /// <transition id>`.
    pub fn permission(&self, transition: &Transition) -> String {
        format!("use {} transition {}", self.id, transition.id)
    }

    /// Whether `user` may move an item from `from` to `to`: the workflow has
    /// the transition and the user holds its permission. An administrator
    /// holds every permission; the kernel's background principal acts for the
    /// site.
    pub fn may(&self, from: &str, to: &str, user: &UserContext) -> bool {
        self.transition(from, to)
            .is_some_and(|t| user.is_background() || user.can(&self.permission(t)))
    }

    /// The states `user` may move an item in state `from` to, in form order.
    pub fn targets(&self, from: &str, user: &UserContext) -> Vec<&State> {
        self.states
            .iter()
            .filter(|s| self.may(from, &s.id, user))
            .collect()
    }

    /// The state a save goes to when it names none: the current one if the
    /// user may stay there, else the first they may reach. `None` when they
    /// may make no transition at all.
    pub fn default_target(&self, from: &str, user: &UserContext) -> Option<String> {
        let targets = self.targets(from, user);
        targets
            .iter()
            .find(|s| s.id == from)
            .or_else(|| targets.first())
            .map(|s| s.id.clone())
    }

    /// Whether an item in state `id` is published. A state the workflow does
    /// not have is unpublished.
    pub fn is_published(&self, id: &str) -> bool {
        self.state(id).is_some_and(|s| s.published)
    }

    /// The published status an item in state `id` has.
    pub fn status_of(&self, id: &str) -> i16 {
        i16::from(self.is_published(id))
    }

    /// The state of a revision: what it stored, or for a revision saved
    /// before the type was moderated (or in a state this workflow no longer
    /// has), the first published state if it was published and the default
    /// state if not.
    pub fn state_of(&self, stored: Option<&str>, status: i16) -> String {
        if let Some(state) = stored.and_then(|id| self.state(id)) {
            return state.id.clone();
        }
        if status == 1 {
            if let Some(state) = self.states.iter().find(|s| s.published) {
                return state.id.clone();
            }
        }
        self.default_state.clone()
    }
}

/// The three-state workflow every Drupal site with content moderation starts
/// from, and what an upgraded site is seeded with: Draft, Published and
/// Archived, moved between by the five transitions of Drupal's Editorial
/// workflow, named the same so a migrated role keeps its grants.
pub fn builtin_editorial() -> ModerationWorkflow {
    ModerationWorkflow {
        id: EDITORIAL.to_string(),
        label: "Editorial".to_string(),
        default_state: DRAFT.to_string(),
        states: vec![
            State {
                id: DRAFT.to_string(),
                label: "Draft".to_string(),
                published: false,
                default_revision: false,
            },
            State {
                id: PUBLISHED.to_string(),
                label: "Published".to_string(),
                published: true,
                default_revision: true,
            },
            State {
                id: ARCHIVED.to_string(),
                label: "Archived".to_string(),
                published: false,
                default_revision: true,
            },
        ],
        transitions: vec![
            Transition {
                id: "create_new_draft".to_string(),
                label: "Create New Draft".to_string(),
                from: vec![DRAFT.to_string(), PUBLISHED.to_string()],
                to: DRAFT.to_string(),
            },
            Transition {
                id: "publish".to_string(),
                label: "Publish".to_string(),
                from: vec![DRAFT.to_string(), PUBLISHED.to_string()],
                to: PUBLISHED.to_string(),
            },
            Transition {
                id: "archive".to_string(),
                label: "Archive".to_string(),
                from: vec![PUBLISHED.to_string()],
                to: ARCHIVED.to_string(),
            },
            Transition {
                id: "archived_draft".to_string(),
                label: "Restore to Draft".to_string(),
                from: vec![ARCHIVED.to_string()],
                to: DRAFT.to_string(),
            },
            Transition {
                id: "archived_published".to_string(),
                label: "Restore".to_string(),
                from: vec![ARCHIVED.to_string()],
                to: PUBLISHED.to_string(),
            },
        ],
    }
}

/// A best-effort label for state `id` with no workflow in hand: Draft,
/// Published and Archived read as the built-in workflow has them; anything
/// else is title-cased from its machine name. Used only where a real
/// workflow is not available ([`crate::content::WriteDenied`]'s `Display`);
/// every screen uses the real workflow's [`ModerationWorkflow::label`]
/// instead.
pub fn display_label(id: &str) -> String {
    match id {
        DRAFT => "Draft".to_string(),
        PUBLISHED => "Published".to_string(),
        ARCHIVED => "Archived".to_string(),
        _ => id
            .split('_')
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Whether a content type's settings turn moderation on.
pub fn is_moderated(settings: &serde_json::Value) -> bool {
    settings
        .get(SETTING)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// The workflow id a moderated type's settings name: [`EDITORIAL`] when the
/// setting is absent or not a string.
pub fn workflow_setting(settings: &serde_json::Value) -> &str {
    settings
        .get(WORKFLOW_SETTING)
        .and_then(serde_json::Value::as_str)
        .unwrap_or(EDITORIAL)
}

/// Every workflow a `moderation.workflows` variable holds, read fresh on
/// every use (never cached), as [`crate::content::item_service::ItemService::stage_view`]
/// reads the stage workflow.
///
/// `None`, an unparsable value or an empty list all fall back to
/// `vec![builtin_editorial()]`, logged on a value that does not parse: an
/// upgraded site reads exactly what it had before, and a corrupt variable
/// never takes moderation away from a type that turns it on.
pub fn resolve_workflows(value: Option<&serde_json::Value>) -> Vec<ModerationWorkflow> {
    let Some(value) = value else {
        return vec![builtin_editorial()];
    };
    match serde_json::from_value::<RawWorkflows>(value.clone()) {
        Ok(raw) if !raw.workflows.is_empty() => raw.workflows,
        Ok(_) => vec![builtin_editorial()],
        Err(e) => {
            warn!(
                error = %e,
                "ignoring an unreadable `moderation.workflows` variable: using the built-in editorial workflow"
            );
            vec![builtin_editorial()]
        }
    }
}

/// The workflow named `id` among `workflows`, if there is one.
pub fn workflow_by_id<'a>(
    workflows: &'a [ModerationWorkflow],
    id: &str,
) -> Option<&'a ModerationWorkflow> {
    workflows.iter().find(|w| w.id == id)
}

/// Whether `id` is a valid state machine name: at most 32 characters
/// (`item_revision.moderation_state` is `VARCHAR(32)`), matching
/// `^[a-z][a-z0-9_]*$`.
fn valid_state_id(id: &str) -> bool {
    id.len() <= 32
        && id.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// What is wrong with a `moderation.workflows` value `config import` is about
/// to store, one message per problem; empty when it is valid.
///
/// Checked here: the value parses; every state id is a valid machine name;
/// every workflow names a `default_state` it has and has at least one
/// published state; every transition's `from` and `to` name states the
/// workflow has; a published state has `default_revision: true` (publishing
/// always replaces what visitors see); and no two workflows in the same value
/// give the same state id different `published` or `default_revision` flags
/// (both exist at run time keyed only by id, so they must agree wherever a
/// site reuses one).
pub fn problems(value: &serde_json::Value) -> Vec<String> {
    let raw = match serde_json::from_value::<RawWorkflows>(value.clone()) {
        Ok(raw) => raw,
        Err(e) => {
            return vec![format!(
                "`{WORKFLOWS_KEY}` must be {{workflows: [...]}}: {e}"
            )];
        }
    };

    let mut problems = Vec::new();
    let mut seen_states: HashMap<&str, (bool, bool)> = HashMap::new();

    for workflow in &raw.workflows {
        let at = |what: &str| format!("workflow '{}': {what}", workflow.id);
        let state_ids: std::collections::HashSet<&str> =
            workflow.states.iter().map(|s| s.id.as_str()).collect();

        for state in &workflow.states {
            if !valid_state_id(&state.id) {
                problems.push(at(&format!(
                    "state '{}' must be at most 32 characters and match ^[a-z][a-z0-9_]*$",
                    state.id
                )));
            }
            if state.published && !state.default_revision {
                problems.push(at(&format!(
                    "state '{}' is published but not default_revision: a published state must replace the item row",
                    state.id
                )));
            }
            match seen_states.get(state.id.as_str()) {
                Some(&flags) if flags != (state.published, state.default_revision) => {
                    problems.push(format!(
                        "state '{}' is published={}/default_revision={} in one workflow and published={}/default_revision={} in another: the same state id must mean the same thing everywhere",
                        state.id, flags.0, flags.1, state.published, state.default_revision
                    ));
                }
                _ => {
                    seen_states.insert(&state.id, (state.published, state.default_revision));
                }
            }
        }

        if !state_ids.contains(workflow.default_state.as_str()) {
            problems.push(at(&format!(
                "default_state '{}' is not one of its states",
                workflow.default_state
            )));
        }
        if !workflow.states.iter().any(|s| s.published) {
            problems.push(at("has no published state"));
        }

        for (n, t) in workflow.transitions.iter().enumerate() {
            let at_t = |what: &str| {
                format!(
                    "workflow '{}' transition {} ({}): {what}",
                    workflow.id,
                    n + 1,
                    t.id
                )
            };
            for from in &t.from {
                if !state_ids.contains(from.as_str()) {
                    problems.push(at_t(&format!("no state '{from}' to move from")));
                }
            }
            if !state_ids.contains(t.to.as_str()) {
                problems.push(at_t(&format!("no state '{}' to move to", t.to)));
            }
        }
    }

    problems
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    fn holding(workflow: &str, transitions: &[&str]) -> UserContext {
        UserContext::authenticated(
            Uuid::now_v7(),
            transitions
                .iter()
                .map(|t| format!("use {workflow} transition {t}"))
                .collect(),
        )
    }

    /// Umami's three roles: an author holds Create New Draft only; an editor
    /// every transition but Restore to Draft; an administrator all of them.
    fn roles() -> Vec<(&'static str, UserContext)> {
        vec![
            ("author", holding(EDITORIAL, &["create_new_draft"])),
            (
                "editor",
                holding(
                    EDITORIAL,
                    &[
                        "create_new_draft",
                        "publish",
                        "archive",
                        "archived_published",
                    ],
                ),
            ),
            ("admin", UserContext::administrator(Uuid::nil(), vec![])),
            ("nobody", holding(EDITORIAL, &[])),
        ]
    }

    /// Every (from, to) pair for every role: exactly the transitions the role
    /// holds are allowed, and every other pair is refused.
    #[test]
    fn every_transition_is_allowed_or_refused_per_role() {
        let workflow = builtin_editorial();
        let states = [DRAFT, PUBLISHED, ARCHIVED];
        let allowed: &[(&str, &str, &str)] = &[
            ("author", DRAFT, DRAFT),
            ("author", PUBLISHED, DRAFT),
            ("editor", DRAFT, DRAFT),
            ("editor", PUBLISHED, DRAFT),
            ("editor", DRAFT, PUBLISHED),
            ("editor", PUBLISHED, PUBLISHED),
            ("editor", PUBLISHED, ARCHIVED),
            ("editor", ARCHIVED, PUBLISHED),
            ("admin", DRAFT, DRAFT),
            ("admin", PUBLISHED, DRAFT),
            ("admin", DRAFT, PUBLISHED),
            ("admin", PUBLISHED, PUBLISHED),
            ("admin", PUBLISHED, ARCHIVED),
            ("admin", ARCHIVED, PUBLISHED),
            ("admin", ARCHIVED, DRAFT),
        ];
        for (role, user) in roles() {
            for from in states {
                for to in states {
                    let want = allowed.contains(&(role, from, to));
                    assert_eq!(
                        workflow.may(from, to, &user),
                        want,
                        "{role}: {from} -> {to} should be {}",
                        if want { "allowed" } else { "refused" }
                    );
                }
            }
        }
    }

    #[test]
    fn a_pair_without_a_transition_is_refused_even_to_an_administrator() {
        let workflow = builtin_editorial();
        let admin = UserContext::administrator(Uuid::nil(), vec![]);
        assert!(!workflow.may(DRAFT, ARCHIVED, &admin));
        assert!(!workflow.may(ARCHIVED, ARCHIVED, &admin));
        assert!(!workflow.may("unknown", PUBLISHED, &admin));
        assert!(!workflow.may(DRAFT, "unknown", &admin));
    }

    #[test]
    fn the_form_offers_the_reachable_states_and_defaults_to_staying() {
        let workflow = builtin_editorial();
        let author = holding(EDITORIAL, &["create_new_draft"]);
        let ids = |states: Vec<&State>| states.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(workflow.targets(DRAFT, &author)), vec![DRAFT]);
        assert_eq!(
            workflow.default_target(PUBLISHED, &author),
            Some(DRAFT.to_string())
        );
        assert!(workflow.targets(ARCHIVED, &author).is_empty());
        assert_eq!(workflow.default_target(ARCHIVED, &author), None);

        let editor = holding(
            EDITORIAL,
            &[
                "create_new_draft",
                "publish",
                "archive",
                "archived_published",
            ],
        );
        assert_eq!(
            ids(workflow.targets(PUBLISHED, &editor)),
            vec![DRAFT, PUBLISHED, ARCHIVED]
        );
        assert_eq!(
            workflow.default_target(PUBLISHED, &editor),
            Some(PUBLISHED.to_string())
        );
        assert_eq!(ids(workflow.targets(ARCHIVED, &editor)), vec![PUBLISHED]);
    }

    #[test]
    fn a_revision_saved_before_moderation_reads_from_its_status() {
        let workflow = builtin_editorial();
        assert_eq!(workflow.state_of(None, 1), PUBLISHED);
        assert_eq!(workflow.state_of(None, 0), DRAFT);
        assert_eq!(workflow.state_of(Some(ARCHIVED), 0), ARCHIVED);
        assert_eq!(workflow.state_of(Some("bogus"), 1), PUBLISHED);
        assert_eq!(workflow.status_of(PUBLISHED), 1);
        assert_eq!(workflow.status_of(DRAFT), 0);
        assert_eq!(workflow.status_of(ARCHIVED), 0);
    }

    #[test]
    fn the_permission_names_are_drupals() {
        let workflow = builtin_editorial();
        let names: Vec<String> = workflow
            .transitions
            .iter()
            .map(|t| workflow.permission(t))
            .collect();
        assert_eq!(
            names,
            vec![
                "use editorial transition create_new_draft",
                "use editorial transition publish",
                "use editorial transition archive",
                "use editorial transition archived_draft",
                "use editorial transition archived_published",
            ]
        );
    }

    #[test]
    fn resolve_falls_back_to_the_built_in_workflow() {
        assert_eq!(resolve_workflows(None), vec![builtin_editorial()]);
        assert_eq!(
            resolve_workflows(Some(&json!("junk"))),
            vec![builtin_editorial()]
        );
        assert_eq!(
            resolve_workflows(Some(&json!({"workflows": []}))),
            vec![builtin_editorial()]
        );
    }

    fn review_workflow() -> serde_json::Value {
        json!({
            "workflows": [{
                "id": "review",
                "label": "Review",
                "default_state": "draft",
                "states": [
                    {"id": "draft", "label": "Draft", "published": false, "default_revision": false},
                    {"id": "in_review", "label": "In review", "published": false, "default_revision": false},
                    {"id": "approved", "label": "Approved", "published": false, "default_revision": false},
                    {"id": "published", "label": "Published", "published": true, "default_revision": true},
                    {"id": "archived", "label": "Archived", "published": false, "default_revision": true}
                ],
                "transitions": [
                    {"id": "submit", "label": "Submit for review", "from": ["draft"], "to": "in_review"},
                    {"id": "approve", "label": "Approve", "from": ["in_review"], "to": "approved"},
                    {"id": "publish", "label": "Publish", "from": ["approved"], "to": "published"},
                    {"id": "archive", "label": "Archive", "from": ["published"], "to": "archived"}
                ]
            }]
        })
    }

    #[test]
    fn a_well_formed_workflow_has_no_problems() {
        assert_eq!(problems(&review_workflow()), Vec::<String>::new());
    }

    #[test]
    fn problems_names_what_is_wrong() {
        assert!(problems(&json!("junk"))[0].contains("workflows"));

        let mut bad = review_workflow();
        bad["workflows"][0]["transitions"][0]["to"] = json!("nowhere");
        let found = problems(&bad);
        assert!(
            found.iter().any(|p| p.contains("no state 'nowhere'")),
            "{found:?}"
        );

        let mut bad = review_workflow();
        bad["workflows"][0]["states"] = json!([
            {"id": "draft", "label": "Draft", "published": false, "default_revision": false}
        ]);
        let found = problems(&bad);
        assert!(
            found.iter().any(|p| p.contains("no published state")),
            "{found:?}"
        );

        let mut bad = review_workflow();
        bad["workflows"][0]["default_state"] = json!("missing");
        let found = problems(&bad);
        assert!(
            found.iter().any(|p| p.contains("default_state")),
            "{found:?}"
        );

        let mut bad = review_workflow();
        bad["workflows"][0]["states"][3]["default_revision"] = json!(false);
        let found = problems(&bad);
        assert!(
            found
                .iter()
                .any(|p| p.contains("published but not default_revision")),
            "{found:?}"
        );

        let mut bad = review_workflow();
        bad["workflows"][0]["states"][0]["id"] = json!("Not-Valid!");
        let found = problems(&bad);
        assert!(
            found.iter().any(|p| p.contains("^[a-z][a-z0-9_]*$")),
            "{found:?}"
        );

        let long_id = "a".repeat(33);
        let mut bad = review_workflow();
        bad["workflows"][0]["states"][0]["id"] = json!(long_id);
        let found = problems(&bad);
        assert!(
            found.iter().any(|p| p.contains("32 characters")),
            "{found:?}"
        );

        let mut two = json!({"workflows": [review_workflow()["workflows"][0].clone(), review_workflow()["workflows"][0].clone()]});
        two["workflows"][1]["id"] = json!("review_two");
        two["workflows"][1]["states"][0]["published"] = json!(true);
        two["workflows"][1]["states"][0]["default_revision"] = json!(true);
        let found = problems(&two);
        assert!(
            found
                .iter()
                .any(|p| p.contains("mean the same thing everywhere")),
            "{found:?}"
        );
    }
}
