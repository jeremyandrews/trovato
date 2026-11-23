//! Content moderation: the editorial workflow of one item (B158).
//!
//! A content type whose settings say `moderation: true` is moderated. Each of
//! its revisions carries a state, `item_revision.moderation_state`: Draft,
//! Published or Archived. A save is a transition from the state of the item's
//! latest revision to the state the user asks for, and each transition is
//! granted by its own permission, named as Drupal's Editorial workflow names
//! it (`use editorial transition publish`), so a migrated role keeps its
//! grants.
//!
//! The item row is the item's default revision, what visitors see, and stays
//! as it is when a published item is saved as a Draft: that save writes only a
//! revision (a forward draft), and the item row changes when a later
//! transition publishes or archives. The read path never asks about
//! moderation.
//!
//! The states and transitions are fixed, the Editorial workflow every Drupal
//! site with content moderation starts from; a type is moderated or not. This
//! module is pure: [`ItemService`](super::ItemService) applies it on every
//! write path.

use crate::tap::UserContext;

/// A revision being worked on, not visible to visitors.
pub const DRAFT: &str = "draft";
/// The revision visitors see.
pub const PUBLISHED: &str = "published";
/// Taken down: unpublished, kept.
pub const ARCHIVED: &str = "archived";

/// Every state, in the order a form offers them.
pub const STATES: &[&str] = &[DRAFT, PUBLISHED, ARCHIVED];

/// The type setting that turns moderation on.
pub const SETTING: &str = "moderation";

/// The permission that lets a user read the latest revision of an item they
/// may view, when it is a draft ahead of the published one, and the moderated
/// content list (Drupal's `view latest version`).
pub const VIEW_LATEST_VERSION: &str = "view latest version";

/// One transition of the workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    /// The machine name, which the permission carries.
    pub id: &'static str,
    /// What a person calls it.
    pub label: &'static str,
    /// The states it leaves from.
    pub from: &'static [&'static str],
    /// The state it arrives at.
    pub to: &'static str,
}

/// The Editorial workflow's transitions.
pub const TRANSITIONS: &[Transition] = &[
    Transition {
        id: "create_new_draft",
        label: "Create New Draft",
        from: &[DRAFT, PUBLISHED],
        to: DRAFT,
    },
    Transition {
        id: "publish",
        label: "Publish",
        from: &[DRAFT, PUBLISHED],
        to: PUBLISHED,
    },
    Transition {
        id: "archive",
        label: "Archive",
        from: &[PUBLISHED],
        to: ARCHIVED,
    },
    Transition {
        id: "archived_draft",
        label: "Restore to Draft",
        from: &[ARCHIVED],
        to: DRAFT,
    },
    Transition {
        id: "archived_published",
        label: "Restore",
        from: &[ARCHIVED],
        to: PUBLISHED,
    },
];

/// The permission that grants `transition`.
pub fn permission(transition: &Transition) -> String {
    format!("use editorial transition {}", transition.id)
}

/// Every transition permission, for the permission list.
pub const TRANSITION_PERMISSIONS: &[&str] = &[
    "use editorial transition create_new_draft",
    "use editorial transition publish",
    "use editorial transition archive",
    "use editorial transition archived_draft",
    "use editorial transition archived_published",
];

/// The state named `name`, as the module's own `&'static str`, if it is one.
pub fn state(name: &str) -> Option<&'static str> {
    STATES.iter().copied().find(|s| *s == name)
}

/// What a person calls a state.
pub fn label(state: &str) -> &'static str {
    match state {
        DRAFT => "Draft",
        PUBLISHED => "Published",
        ARCHIVED => "Archived",
        _ => "Unknown",
    }
}

/// The transition from `from` to `to`, if the workflow has one.
pub fn transition(from: &str, to: &str) -> Option<&'static Transition> {
    TRANSITIONS
        .iter()
        .find(|t| t.to == to && t.from.contains(&from))
}

/// Whether `user` may move an item from `from` to `to`: the workflow has the
/// transition and the user holds its permission. An administrator holds every
/// permission; the kernel's background principal acts for the site.
pub fn may(from: &str, to: &str, user: &UserContext) -> bool {
    transition(from, to).is_some_and(|t| user.is_background() || user.can(&permission(t)))
}

/// The states `user` may move an item in state `from` to, in form order.
pub fn targets(from: &str, user: &UserContext) -> Vec<&'static str> {
    STATES
        .iter()
        .copied()
        .filter(|to| may(from, to, user))
        .collect()
}

/// The state a save goes to when it names none: the current one if the user
/// may stay there, else the first they may reach. `None` when they may make
/// no transition at all.
pub fn default_target(from: &str, user: &UserContext) -> Option<&'static str> {
    let targets = targets(from, user);
    targets
        .iter()
        .copied()
        .find(|t| *t == from)
        .or_else(|| targets.first().copied())
}

/// The state of a revision: what it stored, or for a revision saved before
/// the type was moderated, Published if it was published and Draft if not.
pub fn state_of(stored: Option<&str>, status: i16) -> &'static str {
    stored
        .and_then(state)
        .unwrap_or(if status == 1 { PUBLISHED } else { DRAFT })
}

/// The published status an item in `state` has.
pub fn status_of(state: &str) -> i16 {
    i16::from(state == PUBLISHED)
}

/// Whether a content type's settings turn moderation on.
pub fn is_moderated(settings: &serde_json::Value) -> bool {
    settings
        .get(SETTING)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn holding(transitions: &[&str]) -> UserContext {
        UserContext::authenticated(
            Uuid::now_v7(),
            transitions
                .iter()
                .map(|t| format!("use editorial transition {t}"))
                .collect(),
        )
    }

    /// Umami's three roles: an author holds Create New Draft only; an editor
    /// every transition but Restore to Draft; an administrator all of them.
    fn roles() -> Vec<(&'static str, UserContext)> {
        vec![
            ("author", holding(&["create_new_draft"])),
            (
                "editor",
                holding(&[
                    "create_new_draft",
                    "publish",
                    "archive",
                    "archived_published",
                ]),
            ),
            ("admin", UserContext::administrator(Uuid::nil(), vec![])),
            ("nobody", holding(&[])),
        ]
    }

    /// Every (from, to) pair for every role: exactly the transitions the role
    /// holds are allowed, and every other pair is refused.
    #[test]
    fn every_transition_is_allowed_or_refused_per_role() {
        // (role, from, to) that are allowed; everything else is refused.
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
            for from in STATES {
                for to in STATES {
                    let want = allowed.contains(&(role, *from, *to));
                    assert_eq!(
                        may(from, to, &user),
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
        let admin = UserContext::administrator(Uuid::nil(), vec![]);
        assert!(!may(DRAFT, ARCHIVED, &admin));
        assert!(!may(ARCHIVED, ARCHIVED, &admin));
        assert!(!may("unknown", PUBLISHED, &admin));
        assert!(!may(DRAFT, "unknown", &admin));
    }

    #[test]
    fn the_form_offers_the_reachable_states_and_defaults_to_staying() {
        let author = holding(&["create_new_draft"]);
        assert_eq!(targets(DRAFT, &author), vec![DRAFT]);
        // An author's edit of a published item becomes a draft.
        assert_eq!(default_target(PUBLISHED, &author), Some(DRAFT));
        assert_eq!(targets(ARCHIVED, &author), Vec::<&str>::new());
        assert_eq!(default_target(ARCHIVED, &author), None);

        let editor = holding(&[
            "create_new_draft",
            "publish",
            "archive",
            "archived_published",
        ]);
        assert_eq!(
            targets(PUBLISHED, &editor),
            vec![DRAFT, PUBLISHED, ARCHIVED]
        );
        assert_eq!(default_target(PUBLISHED, &editor), Some(PUBLISHED));
        // Restore, not Restore to Draft.
        assert_eq!(targets(ARCHIVED, &editor), vec![PUBLISHED]);
    }

    #[test]
    fn a_revision_saved_before_moderation_reads_from_its_status() {
        assert_eq!(state_of(None, 1), PUBLISHED);
        assert_eq!(state_of(None, 0), DRAFT);
        assert_eq!(state_of(Some(ARCHIVED), 0), ARCHIVED);
        assert_eq!(state_of(Some("bogus"), 1), PUBLISHED);
        assert_eq!(status_of(PUBLISHED), 1);
        assert_eq!(status_of(DRAFT), 0);
        assert_eq!(status_of(ARCHIVED), 0);
    }

    #[test]
    fn the_permission_names_are_drupals() {
        let names: Vec<String> = TRANSITIONS.iter().map(permission).collect();
        assert_eq!(names, TRANSITION_PERMISSIONS);
    }
}
