//! Content moderation: configurable editorial workflows (B158, B301).
//!
//! A content type whose settings say `moderation: true` is moderated, and its
//! `moderation_workflow` setting names the [`ModerationWorkflow`] its items
//! move through, defaulting to [`EDITORIAL`] (Draft, Published, Archived) when
//! the setting is absent. Every stored workflow lives in one site
//! configuration variable, [`WORKFLOWS_KEY`]; [`resolve_workflows`] turns it
//! into typed workflows, falling back to [`builtin_editorial`] when the
//! variable is absent or unreadable, so a previous release's data (which never
//! wrote the variable) still reads as moderated with the workflow it always
//! had. [`problems`] is the stricter check `config import` applies before a
//! value is ever stored.
//!
//! A revision carries a state, `item_revision.moderation_state`, one of the
//! workflow's own state ids. A save is a transition from the state of the
//! item's latest revision to the state the user asks for, and each transition
//! is granted by its own permission, named `use <workflow id> transition
//! <transition id>`; the built-in workflow's id is `editorial`, so a migrated
//! role's `use editorial transition publish` grant keeps meaning what it did.
//!
//! A state's `published` flag decides the item's published status; its
//! `default_revision` flag decides whether a save into it over a published
//! item moves the item row (an archive, `default_revision: true`) or only adds
//! a revision beside it (a forward draft, `default_revision: false`) — the
//! item row, what visitors see, stays as it is. A published state must also be
//! a default-revision state: there is no such thing as a published forward
//! draft. The read path never asks about moderation.
//!
//! This module is pure: [`ItemService`](super::ItemService) applies it on
//! every write path, and reads the workflow a type uses on each call, not
//! cached — a workflow change is seen at once by every server, as the stage
//! workflow is (`docs/design/stage-workflow.md`). See
//! `docs/design/moderation-workflows.md`.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use tracing::warn;

use crate::tap::UserContext;

/// A revision being worked on, not visible to visitors: the built-in
/// workflow's starting state.
pub const DRAFT: &str = "draft";
/// The revision visitors see: the built-in workflow's one published state.
pub const PUBLISHED: &str = "published";
/// Taken down: unpublished, kept: the built-in workflow's other
/// default-revision state.
pub const ARCHIVED: &str = "archived";

/// The built-in workflow's id, kept so a role granted `use editorial
/// transition ...` before workflows were configurable keeps meaning the same
/// thing.
pub const EDITORIAL: &str = "editorial";

/// The type setting that turns moderation on.
pub const SETTING: &str = "moderation";

/// The type setting naming which stored workflow its items use; absent means
/// [`EDITORIAL`].
pub const WORKFLOW_SETTING: &str = "moderation_workflow";

/// The site configuration key every moderation workflow is stored under, one
/// value holding every workflow (`{workflows: [...]}`).
pub const WORKFLOWS_KEY: &str = "moderation.workflows";

/// The permission that lets a user read the latest revision of an item they
/// may view, when it is ahead of the published one, and the moderated content
/// list (Drupal's `view latest version`). Not workflow-specific: one
/// permission covers every moderated type's drafts.
pub const VIEW_LATEST_VERSION: &str = "view latest version";

/// Maximum length of a state id ([`problems`], [`is_valid_state_id`]).
const MAX_STATE_ID_LEN: usize = 32;

/// One state an item can be in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// The machine name, stored in `item_revision.moderation_state` and
    /// carried in the transition permission.
    pub id: String,
    /// What a person calls it.
    pub label: String,
    /// Whether an item in this state is published (`item.status = 1`).
    pub published: bool,
    /// Whether a save into this state over a published item moves the item
    /// row. `false` is a forward draft: a revision beside the published one,
    /// which visitors never see until it is itself published. A published
    /// state is always `true`: there is no published forward draft.
    pub default_revision: bool,
}

/// One transition of a workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// A site's own content moderation workflow: its states, its transitions, and
/// which state a new item starts in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationWorkflow {
    /// The machine name, carried in every transition permission
    /// (`use <id> transition <transition id>`).
    pub id: String,
    /// What a person calls it.
    pub label: String,
    /// The state a new item starts in.
    pub default_state: String,
    /// Every state, in the order a form offers them.
    pub states: Vec<State>,
    /// Every transition.
    pub transitions: Vec<Transition>,
}

impl ModerationWorkflow {
    /// The state named `id`, if this workflow has one.
    pub fn state(&self, id: &str) -> Option<&State> {
        self.states.iter().find(|s| s.id == id)
    }

    /// What a person calls the state named `id`; `"Unknown"` when this
    /// workflow has no such state.
    pub fn label(&self, id: &str) -> String {
        self.state(id)
            .map(|s| s.label.clone())
            .unwrap_or_else(|| "Unknown".to_string())
    }

    /// The transition from `from` to `to`, if this workflow has one.
    pub fn transition(&self, from: &str, to: &str) -> Option<&Transition> {
        self.transitions
            .iter()
            .find(|t| t.to == to && t.from.iter().any(|f| f == from))
    }

    /// The permission that grants `transition` on this workflow.
    pub fn permission(&self, transition: &Transition) -> String {
        format!("use {} transition {}", self.id, transition.id)
    }

    /// Every transition's permission, for the permission grid and
    /// `config import` (`use <id> transition <transition id>`).
    pub fn transition_permissions(&self) -> Vec<String> {
        self.transitions.iter().map(|t| self.permission(t)).collect()
    }

    /// Whether `user` may move an item from `from` to `to`: this workflow has
    /// the transition and the user holds its permission. An administrator
    /// holds every permission; the kernel's background principal acts for the
    /// site.
    pub fn may(&self, from: &str, to: &str, user: &UserContext) -> bool {
        self.transition(from, to)
            .is_some_and(|t| user.is_background() || user.can(&self.permission(t)))
    }

    /// The states `user` may move an item in state `from` to, in form order.
    pub fn targets(&self, from: &str, user: &UserContext) -> Vec<String> {
        self.states
            .iter()
            .map(|s| s.id.clone())
            .filter(|to| self.may(from, to, user))
            .collect()
    }

    /// The state a save goes to when it names none: the current one if the
    /// user may stay there, else the first they may reach. `None` when they
    /// may make no transition at all.
    pub fn default_target(&self, from: &str, user: &UserContext) -> Option<String> {
        let targets = self.targets(from, user);
        targets
            .iter()
            .find(|t| t.as_str() == from)
            .cloned()
            .or_else(|| targets.first().cloned())
    }

    /// The state of a revision: what it stored, if this workflow has that
    /// state, or for a revision saved before the type was moderated (or
    /// naming a state this workflow has since dropped), the first published
    /// state if it was published and the default state if not.
    pub fn state_of(&self, stored: Option<&str>, status: i16) -> String {
        if let Some(stored) = stored
            && self.state(stored).is_some()
        {
            return stored.to_string();
        }
        if status == 1 {
            self.states
                .iter()
                .find(|s| s.published)
                .map(|s| s.id.clone())
                .unwrap_or_else(|| self.default_state.clone())
        } else {
            self.default_state.clone()
        }
    }

    /// The published status an item in `state` has; unpublished if this
    /// workflow has no such state.
    pub fn status_of(&self, state: &str) -> i16 {
        i16::from(self.state(state).is_some_and(|s| s.published))
    }

    /// Whether a save that moves an existing, published item to `to` is a
    /// forward draft: a revision beside the published one, leaving the item
    /// row as it is.
    pub fn is_forward_revision(&self, to: &str, existing_published: bool) -> bool {
        existing_published && self.state(to).is_some_and(|s| !s.default_revision)
    }
}

/// The workflow every site starts with: Draft, Published, Archived, the
/// permissions named as Drupal's Editorial workflow names them
/// (`use editorial transition publish`), so a migrated role's grants keep
/// meaning the same thing.
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

/// Whether a content type's settings turn moderation on.
pub fn is_moderated(settings: &serde_json::Value) -> bool {
    settings
        .get(SETTING)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// The workflow id a content type's settings name, `editorial` when the
/// setting is absent.
pub fn workflow_name(settings: &serde_json::Value) -> &str {
    settings
        .get(WORKFLOW_SETTING)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(EDITORIAL)
}

/// A state id: at most 32 characters, `^[a-z][a-z0-9_]*$`.
fn is_valid_state_id(id: &str) -> bool {
    id.len() <= MAX_STATE_ID_LEN
        && id
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[derive(Debug, Clone, Deserialize)]
struct RawState {
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    published: bool,
    #[serde(default)]
    default_revision: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct RawTransition {
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    from: Vec<String>,
    to: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RawWorkflow {
    id: String,
    #[serde(default)]
    label: String,
    default_state: String,
    #[serde(default)]
    states: Vec<RawState>,
    #[serde(default)]
    transitions: Vec<RawTransition>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RawWorkflows {
    #[serde(default)]
    workflows: Vec<RawWorkflow>,
}

fn parse(value: &serde_json::Value) -> Result<RawWorkflows, String> {
    serde_json::from_value::<RawWorkflows>(value.clone())
        .map_err(|e| format!("`{WORKFLOWS_KEY}` must be {{workflows: [{{id, default_state, states, transitions}}]}}: {e}"))
}

fn titlecase(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for (i, word) in id.split('_').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.extend(chars);
        }
    }
    out
}

/// Turn one raw workflow into a usable [`ModerationWorkflow`], dropping (with
/// a warning) a transition naming a state it does not have, as
/// [`crate::models::workflow::Workflow::resolve`] drops a stage transition
/// that does not resolve. The workflow itself is dropped, with a warning, when
/// its `default_state` is not one of its states or it ends up with no
/// published state.
fn resolve_one(raw: RawWorkflow) -> Option<ModerationWorkflow> {
    let mut states = Vec::new();
    let mut seen = HashSet::new();
    for s in raw.states {
        if !is_valid_state_id(&s.id) || !seen.insert(s.id.clone()) {
            warn!(workflow = %raw.id, state = %s.id, "ignoring an unusable moderation state");
            continue;
        }
        states.push(State {
            label: if s.label.trim().is_empty() {
                titlecase(&s.id)
            } else {
                s.label
            },
            published: s.published,
            // A published state is always the default revision: there is no
            // published forward draft.
            default_revision: s.default_revision || s.published,
            id: s.id,
        });
    }
    if !states.iter().any(|s| s.id == raw.default_state) {
        warn!(workflow = %raw.id, default_state = %raw.default_state, "ignoring a moderation workflow whose default state is not one of its states");
        return None;
    }
    if !states.iter().any(|s| s.published) {
        warn!(workflow = %raw.id, "ignoring a moderation workflow with no published state");
        return None;
    }
    let state_ids: HashSet<&str> = states.iter().map(|s| s.id.as_str()).collect();
    let transitions: Vec<Transition> = raw
        .transitions
        .into_iter()
        .filter(|t| {
            let usable = !t.from.is_empty()
                && t.from.iter().all(|f| state_ids.contains(f.as_str()))
                && state_ids.contains(t.to.as_str());
            if !usable {
                warn!(workflow = %raw.id, transition = %t.id, "ignoring a moderation transition that names no state");
            }
            usable
        })
        .map(|t| Transition {
            label: if t.label.trim().is_empty() {
                titlecase(&t.id)
            } else {
                t.label
            },
            id: t.id,
            from: t.from,
            to: t.to,
        })
        .collect();
    Some(ModerationWorkflow {
        label: if raw.label.trim().is_empty() {
            titlecase(&raw.id)
        } else {
            raw.label
        },
        id: raw.id,
        default_state: raw.default_state,
        states,
        transitions,
    })
}

/// Every workflow stored in [`WORKFLOWS_KEY`], resolved against the states and
/// transitions each actually has.
///
/// `None` (the variable was never written) or a value that does not parse at
/// all returns `vec![`[`builtin_editorial`]`()]`, logged for the unreadable
/// case: a previous release's data, which never wrote the variable, still
/// reads as moderated with the workflow it always had. A workflow that does
/// not resolve (see [`resolve_one`]) is dropped from the list; if every one
/// is, the built-in workflow is the fallback here too.
pub fn resolve_workflows(value: Option<&serde_json::Value>) -> Vec<ModerationWorkflow> {
    let Some(value) = value else {
        return vec![builtin_editorial()];
    };
    let raw = match parse(value) {
        Ok(raw) => raw,
        Err(e) => {
            warn!(error = %e, "ignoring an unreadable `moderation.workflows`: moderated types use the built-in editorial workflow");
            return vec![builtin_editorial()];
        }
    };
    let workflows: Vec<ModerationWorkflow> =
        raw.workflows.into_iter().filter_map(resolve_one).collect();
    if workflows.is_empty() {
        vec![builtin_editorial()]
    } else {
        workflows
    }
}

/// What is wrong with a `moderation.workflows` value `config import` is about
/// to store, one message per problem; empty when it is valid.
///
/// Checks each workflow on its own (a transition naming an unknown state, no
/// published state, a `default_state` that is not a state, a state id over 32
/// characters or not matching `^[a-z][a-z0-9_]*$`, a duplicate state or
/// transition id within one workflow) and across the whole value (a duplicate
/// workflow id, the same state id carrying different `published` or
/// `default_revision` flags in two workflows — allowed to differ in label
/// only). Whether an item type names a workflow this value does not have, and
/// whether a workflow change drops a state some stored revision still holds,
/// need the database and are checked by
/// `config_storage::yaml::validate_moderation_workflows` instead.
pub fn problems(value: &serde_json::Value) -> Vec<String> {
    let raw = match parse(value) {
        Ok(raw) => raw,
        Err(e) => return vec![e],
    };
    let mut problems = Vec::new();
    let mut workflow_ids = HashSet::new();
    let mut flags: HashMap<String, (bool, bool, String)> = HashMap::new();

    for w in &raw.workflows {
        if !workflow_ids.insert(w.id.clone()) {
            problems.push(format!("workflow '{}' is listed twice", w.id));
        }
        let at = |detail: String| format!("workflow '{}': {detail}", w.id);
        let mut seen_states = HashSet::new();
        let mut has_published = false;
        for s in &w.states {
            if !is_valid_state_id(&s.id) {
                problems.push(at(format!(
                    "state id '{}' must be at most {MAX_STATE_ID_LEN} characters and match ^[a-z][a-z0-9_]*$",
                    s.id
                )));
            }
            if !seen_states.insert(s.id.clone()) {
                problems.push(at(format!("state '{}' is listed twice", s.id)));
            }
            has_published |= s.published;
            let default_revision = s.default_revision || s.published;
            match flags.get(&s.id) {
                Some((published, revision, other)) if (*published, *revision) != (s.published, default_revision) =>
                {
                    problems.push(format!(
                        "state '{}' is published={} default_revision={} in workflow '{other}' but \
                         published={} default_revision={} in workflow '{}'",
                        s.id, published, revision, s.published, default_revision, w.id
                    ));
                }
                _ => {
                    flags.insert(s.id.clone(), (s.published, default_revision, w.id.clone()));
                }
            }
        }
        if w.states.is_empty() {
            problems.push(at("has no states".to_string()));
        }
        if !has_published {
            problems.push(at("has no published state".to_string()));
        }
        if !seen_states.contains(&w.default_state) {
            problems.push(at(format!(
                "default_state '{}' is not one of its states",
                w.default_state
            )));
        }
        let mut seen_transitions = HashSet::new();
        for t in &w.transitions {
            if !seen_transitions.insert(t.id.clone()) {
                problems.push(at(format!("transition '{}' is listed twice", t.id)));
            }
            if t.from.is_empty() {
                problems.push(at(format!(
                    "transition '{}' names no state to move from",
                    t.id
                )));
            }
            for f in &t.from {
                if !seen_states.contains(f) {
                    problems.push(at(format!(
                        "transition '{}' names an unknown state '{f}' to move from",
                        t.id
                    )));
                }
            }
            if !seen_states.contains(&t.to) {
                problems.push(at(format!(
                    "transition '{}' names an unknown state '{}' as its target",
                    t.id, t.to
                )));
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
            ("author", holding("editorial", &["create_new_draft"])),
            (
                "editor",
                holding(
                    "editorial",
                    &["create_new_draft", "publish", "archive", "archived_published"],
                ),
            ),
            ("admin", UserContext::administrator(Uuid::nil(), vec![])),
            ("nobody", holding("editorial", &[])),
        ]
    }

    /// Every (from, to) pair for every role: exactly the transitions the role
    /// holds are allowed, and every other pair is refused.
    #[test]
    fn every_transition_is_allowed_or_refused_per_role() {
        let workflow = builtin_editorial();
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
            for from in [DRAFT, PUBLISHED, ARCHIVED] {
                for to in [DRAFT, PUBLISHED, ARCHIVED] {
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

    /// No role reaches a pair the workflow has no transition for: draft to
    /// archived and archived to archived.
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
        let author = holding("editorial", &["create_new_draft"]);
        assert_eq!(workflow.targets(DRAFT, &author), vec![DRAFT.to_string()]);
        assert_eq!(
            workflow.default_target(PUBLISHED, &author),
            Some(DRAFT.to_string())
        );
        assert_eq!(workflow.targets(ARCHIVED, &author), Vec::<String>::new());
        assert_eq!(workflow.default_target(ARCHIVED, &author), None);

        let editor = holding(
            "editorial",
            &["create_new_draft", "publish", "archive", "archived_published"],
        );
        assert_eq!(
            workflow.targets(PUBLISHED, &editor),
            vec![DRAFT.to_string(), PUBLISHED.to_string(), ARCHIVED.to_string()]
        );
        assert_eq!(
            workflow.default_target(PUBLISHED, &editor),
            Some(PUBLISHED.to_string())
        );
        assert_eq!(workflow.targets(ARCHIVED, &editor), vec![PUBLISHED.to_string()]);
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
        let names: Vec<String> = workflow.transition_permissions();
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
    fn a_published_state_is_always_a_default_revision_state() {
        let workflow = builtin_editorial();
        assert!(workflow.is_forward_revision(DRAFT, true));
        assert!(!workflow.is_forward_revision(ARCHIVED, true));
        assert!(!workflow.is_forward_revision(PUBLISHED, true));
        assert!(!workflow.is_forward_revision(DRAFT, false));
    }

    fn review_workflow_value() -> serde_json::Value {
        json!({
            "workflows": [
                {
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
                        {"id": "archive", "label": "Archive", "from": ["published"], "to": "archived"},
                        {"id": "restore", "label": "Restore", "from": ["archived"], "to": "published"}
                    ]
                }
            ]
        })
    }

    #[test]
    fn a_custom_workflow_resolves_and_its_permissions_name_it() {
        let workflows = resolve_workflows(Some(&review_workflow_value()));
        assert_eq!(workflows.len(), 1);
        let review = &workflows[0];
        assert_eq!(review.id, "review");
        assert_eq!(review.default_state, "draft");
        let submitter = UserContext::authenticated(
            Uuid::now_v7(),
            vec!["use review transition submit".to_string()],
        );
        assert!(review.may("draft", "in_review", &submitter));
        assert!(!review.may("in_review", "approved", &submitter));
        assert_eq!(
            review.permission(review.transition("draft", "in_review").unwrap()),
            "use review transition submit"
        );
    }

    #[test]
    fn no_value_or_an_unreadable_value_falls_back_to_the_built_in_workflow() {
        assert_eq!(resolve_workflows(None), vec![builtin_editorial()]);
        assert_eq!(
            resolve_workflows(Some(&json!("junk"))),
            vec![builtin_editorial()]
        );
    }

    #[test]
    fn problems_are_empty_for_a_well_formed_value() {
        assert!(problems(&review_workflow_value()).is_empty());
    }

    #[test]
    fn problems_name_a_transition_to_an_unknown_state() {
        let value = json!({"workflows": [{
            "id": "w", "default_state": "draft",
            "states": [
                {"id": "draft", "published": false},
                {"id": "published", "published": true},
            ],
            "transitions": [{"id": "t", "from": ["draft"], "to": "nowhere"}],
        }]});
        let found = problems(&value);
        assert!(found.iter().any(|p| p.contains("unknown state 'nowhere'")), "{found:?}");
    }

    #[test]
    fn problems_name_a_workflow_with_no_published_state() {
        let value = json!({"workflows": [{
            "id": "w", "default_state": "draft",
            "states": [{"id": "draft", "published": false}],
            "transitions": [],
        }]});
        assert!(
            problems(&value)
                .iter()
                .any(|p| p.contains("no published state"))
        );
    }

    #[test]
    fn problems_name_a_default_state_that_is_not_a_state() {
        let value = json!({"workflows": [{
            "id": "w", "default_state": "missing",
            "states": [{"id": "draft", "published": false}, {"id": "published", "published": true}],
            "transitions": [],
        }]});
        assert!(
            problems(&value)
                .iter()
                .any(|p| p.contains("default_state 'missing'"))
        );
    }

    #[test]
    fn problems_name_bad_state_ids() {
        let too_long = "a".repeat(33);
        let value = json!({"workflows": [{
            "id": "w", "default_state": "draft",
            "states": [
                {"id": "draft", "published": false},
                {"id": "published", "published": true},
                {"id": too_long, "published": false},
                {"id": "Bad-Id", "published": false},
            ],
            "transitions": [],
        }]});
        let found = problems(&value);
        assert_eq!(
            found.iter().filter(|p| p.contains("must be at most 32 characters")).count(),
            2,
            "{found:?}"
        );
    }

    #[test]
    fn problems_name_a_type_naming_a_workflow_that_does_not_exist() {
        // config_storage::yaml::validate_moderation_workflows checks this one,
        // since it needs the item types in the import; this module's own
        // `problems` only checks one workflow value in isolation.
    }

    #[test]
    fn problems_name_inconsistent_flags_for_the_same_state_id_across_workflows() {
        let value = json!({"workflows": [
            {
                "id": "a", "default_state": "draft",
                "states": [{"id": "draft", "published": false}, {"id": "published", "published": true}],
                "transitions": [],
            },
            {
                "id": "b", "default_state": "draft",
                "states": [
                    {"id": "draft", "published": false, "default_revision": true},
                    {"id": "published", "published": true},
                ],
                "transitions": [],
            },
        ]});
        let found = problems(&value);
        assert!(
            found.iter().any(|p| p.contains("state 'draft'") && p.contains("workflow 'a'") && p.contains("workflow 'b'")),
            "{found:?}"
        );
    }
}
